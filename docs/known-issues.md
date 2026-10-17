# Known issues and explicit limitations

These gaps were carried forward into stack-agent when Chip and FX were consolidated. The
consolidation moved code; it did not close any of these gaps. Each item was checked against the
migrated code. Its status is one of **unresolved**, **partially addressed** or **verified fixed**,
with source references, test evidence and a follow-up. Paths are relative to the repository root.
The tests named here run in CI unless the entry says otherwise.

External references (in `rkendel1/fx` at `b2d11d7`, not copied here):
`docs/rebuild/ZIG_RETIREMENT.md`, `docs/rebuild/ASK_CHIP_ROUTING.md`,
`docs/rebuild/parity/agent-loop.v1.json`. Chip's own register of the same gaps, with roadmap IDs, is
[`product/technical-debt-register.md`](product/technical-debt-register.md) (cited below as `D-nn`).

| | Area | Status |
| --- | --- | --- |
| A | Streaming and CLI contract | Partially addressed: the provider call streams and is normalized; no command streams to its caller; exit codes differ by command |
| B | Cancellation | Partially addressed: model calls only, `serve` only, never confirmed |
| C | Persistence, replay, idempotency | Unresolved (accepted for the first release): the shipped binary persists nothing |
| D | Typed delivery certainty | Verified fixed on the model-call path, with one contract ambiguity |
| E | Completion and verification | Verified as implemented; known completion-evidence limits |
| F | Credentials and transport | Partially addressed: redaction, custom CA and explicit proxy are tested; nothing tested against a real proxy or a live provider |
| G | WASM | Compiles only for the FX/Chip session modules; one decision module is executed under `wasmi` |
| H | Performance | One external three-sample measurement; no reproducible real-model numbers here |
| I | Legacy (Zig) retirement | Separate task; not done; no parity is claimed |

---

## A. Streaming and CLI contract (partially addressed)

**What the code does**

- The native FX provider always streams the provider call. Requests carry `"stream": true`
  (`crates/fx-native-provider/src/openai.rs:118-119`, `crates/fx-native-provider/src/anthropic.rs:64`).
  There is no non-streaming mode.
- A 2xx reply that is not an event stream is refused as `protocol_violation`
  (`crates/fx-native-provider/src/provider.rs:347-361`).
- An event stream cut off before its terminal marker is `IncompleteStream`, and its partial output
  is kept (`provider.rs:377-453`).
- FX assembles the stream into one outcome. Chip receives one `ModelResponse`
  (`crates/chip-fx/src/mapping.rs` `from_outcome`); Chip's FX host has no per-event callback
  (`crates/chip-fx/src/native.rs`).
- **No command streams output to its caller.**
  - `work` and `verify` print one report at the end.
  - The one-shot prompt path prints after the turn.
  - `serve` returns an empty event list until the work ends (`crates/stack-agent/src/service.rs`
    `get_events`; D-18).
- Exit codes are defined in `crates/stack-agent/src/verify.rs:70-74`: 0 verified, 1 not verified,
  2 usage, 3 unavailable, 4 runtime failure. Commands apply them differently:
  - **`work`** returns 3 when the first model call fails before anything ran
    (`software_work.rs`, around `:1715`). A later model failure returns 4. A grounded inspection
    returns 1, never 0.
  - **`verify`** returns 4 for any model failure. It has no first-call special case.
  - **`serve`** returns 2 for bad flags, 3 when a dependency is unavailable, and 4 when it cannot
    bind.
  - **The one-shot prompt** (`stack-agent "<prompt>"`) returns 1 for every error.

**The stand-in mismatch**

When chip-rs made FX the default executor, its performance harness and packaged smoke test still
used model stand-ins that answered plain JSON. The FX native provider refused them ("the response is
not an event stream"), and main went red. chip-rs fixed the stand-ins in 2c22fd8.

Regression coverage in stack-agent:

- `crates/stack-agent/tests/product.rs::a_non_streaming_stand_in_is_refused_explicitly_not_accepted`
  pins today's behavior. A non-streaming stand-in is refused explicitly with `protocol_violation`,
  is not retried, prints no verdict, and exits 3.
- `a_tool_round_trip_runs_chip_over_fx_in_one_process` asserts that the product path sends
  `"stream":true` and completes over an event stream.
- CI runs the packaged smoke test (`scripts/smoke-test.sh`, which uses an event-stream stand-in) on
  the release artifact.

Other evidence:

- `crates/fx-native-provider/tests/conformance.rs`:
  `a_two_hundred_that_is_not_an_event_stream_is_a_protocol_violation`,
  `a_stream_cut_before_its_terminal_marker_keeps_the_partial_and_is_not_a_completion`,
  `events_split_across_reads_and_crlf_line_endings_decode_the_same`.
- `crates/stack-agent/tests/fx_executor.rs` (the whole file runs against an SSE mock).
- `crates/stack-agent/tests/work.rs` tests each exit code.

**Gaps**

- No progress output on any surface.
- Exit codes are not uniform across `work`, `verify`, `serve` and the one-shot prompt.
- `ASK_CHIP_ROUTING.md` (external) records how Zig `fx ask` differs: it streams and collapses every
  failure to exit 1. It also records that `--executor legacy` exits 3, not 2.
- One FX failure-kind decision reads error text: a transport read error counts as a timeout if its
  message contains "timeout" (`provider.rs:416-417`). This affects only the reported kind, not
  delivery certainty.
- The bench harness (`cargo bench -p stack-agent --bench baseline`) only compiles in CI; it does
  not run.

**Follow-up:** decide one exit-code table for every command, with owner sign-off. Then add
event streaming to `serve` (D-18, roadmap P1-O3) and replace the text-based timeout
classification with a typed transport error.

## B. Cancellation (partially addressed)

**The path from a product operation to FX**

1. `serve` receives `POST /v1/work/{id}/cancel` (`crates/stack-agent/src/service.rs`).
2. It sets `RunControl.cancel` (`software_work.rs`, around `:1373`).
3. `CancelBeforeModelCall` (around `:1394`) refuses the next model request. It also wraps each
   call in `chip_fx::with_call_cancel` (`crates/chip-fx/src/native.rs:56-91`).
4. `chip-fx-native` carries the signal onto the blocking provider thread
   (`crates/chip-fx-native/src/lib.rs:273-280`).
5. `fx_core::execute` polls the signal before start (giving `CancelledBeforeStart` and
   `definitely_unsent`) and before each poll (`crates/fx-core/src/executor.rs:313-317, 371-373`).

**Requested is not confirmed**

- A blocking read cannot be interrupted. The native abort returns
  `AbortReport { transport_closed: false }` (`crates/fx-native-provider/src/provider.rs:211-219`).
- The worker stops at its next read or at its deadline (120 s by default). FX reports the
  cancellation as unconfirmed.
- The service reports a running work as `cancellation_requested`, not as cancelled.
- Cancelling never undoes a provider request that may already have been sent. Once a call has
  started it is reported `possibly_sent` or higher, never "unsent".
- Cancelling never undoes a tool side effect that already ran.

**What cannot be cancelled**

- `work` and `verify` have no cancellation input (`control: None`).
- stack-agent installs no SIGINT or SIGTERM handler. A signal ends the process by default action,
  and nothing is reported.
- A tool execution in flight is never interrupted. There is no whole-work deadline (D-16).

**Tests by state**

- **Before inference:**
  - `crates/fx-core/tests/executor.rs::cancellation_before_start_sends_nothing`
  - `crates/fx-single-attempt/tests/adapter.rs::cancellation_before_start_never_reaches_the_port`
  - `crates/fx-native-provider/tests/conformance.rs::cancellation_before_dispatch_never_opens_a_connection`
  - `crates/chip-fx/tests/integrated.rs::cancellation_before_invocation_sends_nothing`
  - `service.rs::queued_work_cancelled_before_admission_never_runs`
- **During inference:**
  - `executor.rs::cancellation_during_the_stream_reports_observed_delivery_and_partial_output`
  - `conformance.rs::cancellation_in_flight_returns_promptly_with_partial_output_and_is_unconfirmed`
  - `integrated.rs::cancellation_after_invocation_is_never_reported_as_unsent`
  - `crates/chip-fx-native/tests/native.rs::a_call_scoped_cancellation_reaches_the_blocking_attempt`
  - `service.rs::cancelling_running_work_is_advisory_and_reported_honestly`. This one uses a
    scripted gated model, not FX.
- **During tool execution:**
  - Only `crates/chip-core/tests/execution.rs::dropping_the_future_cancels_pending_execution`, at
    the executor level.
  - No product-level test. By design the service does not interrupt an execution in flight.
- **After completion:**
  - `service.rs::work_that_finishes_before_cancellation_keeps_its_own_terminal_state` (409)
  - `executor.rs::repeated_cancellation_requests_cancel_once_and_cancel_after_finish_is_ignored`

**Gaps**

- No stack-agent test drives a service cancel into a real FX native call through the binary.
- No CLI cancellation.
- No interruption of an execution in flight.
- No confirmed termination.

**Follow-up:** add a binary-level `serve` cancellation test against a slow event-stream stand-in.
Then decide (owner) on SIGINT handling for `work`, and on a whole-work deadline (D-16, roadmap
P0-04).

## C. Persistence, replay and idempotency (unresolved; accepted for the first release)

- **The shipped binary persists no session, turn, event log or receipt.**
  - `crates/stack-agent` does not depend on `chip-session` or `chip-turns`.
  - `serve` keeps work in memory, bounded by `--max-retained-work` (default 256). Evicted work
    answers 410.
  - All of it is lost when the process exits.
  - `crates/stack-agent/tests/credential_redaction.rs` checks that nothing is written under a fresh
    `HOME`/`XDG_*` during `work`, `serve` and the prompt path.
- **Durable writes that do happen are project state, not agent state:**
  - project files, written by temp file plus rename (`crates/chip-project/src/lib.rs`). A crash
    can leave `.chip-write-*.tmp` behind (D-05).
  - `.chip/graph/` from `init`/`graph`.
  - build artefacts produced by the project's own tests.
- **Durable authority.** None inside stack-agent.
  - `chip-session` can read and write the Zig fx `event_log_v1` layout under a root its caller
    supplies (`docs/product/session-format.md`).
  - `chip-turns` reports `Persistence::{Committed, NotCommitted, Uncertain}`
    (`crates/chip-turns/src/run.rs`).
  - Neither is wired into the binary. This consolidation added no persistence subsystem and makes
    no in-memory state look durable.
- **No retry, resume or replay exists anywhere.** If a process exits or a request's completion is
  uncertain, nothing records it. A new run starts from the beginning, and effects may repeat.
  This is the "RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS" warning that the README, `--help`,
  `work`'s usage text and `docs/product/release-limitations.md` all carry.
  `crates/stack-agent/tests/release_notices.rs` keeps the warning in place.
- **Evidence for the library-level contracts:**
  - `crates/chip-turns/tests/turns.rs` (`a_crash_before_the_watermark_leaves_no_turn_and_nothing_is_run_again`,
    `a_persistence_failure_is_reported_as_not_committed_and_the_work_is_not_repeated`, and others)
  - `crates/chip-session/tests/writer.rs`

**Not claimed:** crash recovery, automatic replay, exactly-once side effects, an idempotence guard.

**Follow-up:** an owner decision on which external authority holds durable work state. Only after
that, wire `chip-turns` (or that authority) into `serve`.

## D. Typed delivery certainty (verified fixed on the model-call path; one ambiguity)

- `chip_inference::Delivery { DefinitelyUnsent, PossiblySent, ConfirmedResponse }` serializes as
  `definitely_unsent` / `possibly_sent` / `confirmed_response`
  (`crates/chip-inference/src/lib.rs:122-140`).
- It is mapped from FX's typed `DeliveryCertainty` field (`crates/chip-fx/src/mapping.rs`). FX
  derives that field from what the attempt observed (`crates/fx-single-attempt/src/mapping.rs:15-19`)
  and never lowers it (`crates/fx-core/src/normalize.rs`).
- No delivery certainty is inferred from error text. There are no retries and no fallback provider.
- **Where it appears:**
  - `WorkOutcome::Failed { delivery }` and the `WorkFailed` event (`crates/chip-core/src/work.rs`)
  - the `"delivery"` key of `work --json`
  - the service's `result.delivery`
- **Tests:**
  - `crates/stack-agent/tests/product.rs::a_failed_model_call_reports_typed_delivery_and_is_not_retried`
    (new): checks the CLI `work --json` field and that there is no second attempt.
  - `service.rs::a_failed_model_call_reports_its_delivery_as_a_field_not_only_text`
  - `crates/chip-fx/tests/integrated.rs::an_ambiguous_delivery_does_not_cause_a_duplicate_provider_call`
  - `crates/chip-inference/tests/provider_neutral_api.rs::delivery_only_moves_toward_sent`
  - `crates/fx-core/tests/executor.rs::delivery_certainty_never_regresses`

**Ambiguity, pinned rather than changed.** FX's contract defines `ConfirmedResponse` as "a response
from the provider was observed" (`crates/fx-core/src/contract.rs:525-535`). But the single-attempt
adapter marks it only once *model output* was observed. So an HTTP error status with a body (401,
500) is reported `possibly_sent`; the CLI output and the new product test show it. This is
conservative: it never understates that a request may have been billed or acted on. It is still
less precise than the contract wording.

**Other gaps:**

- A failure that is not an inference failure carries no delivery. This includes the refusal after
  a cancel request.
- `verify`'s JSON has no delivery field.

**Follow-up:** an owner decision on whether an HTTP error response should be `confirmed_response`.
Make it a change to FX and its tests, not to the migration.

## E. Completion and verification (verified as implemented)

- **A model's completion claim is never accepted on its own.** `chip-core` refuses a `complete`
  decision while a required observation is unmet, or while an answer fails its predicates
  (`crates/chip-core/src/work.rs`, `completion_refused` / `answer_refused`). The work then ends
  blocked.
- **`verified` requires host-side evidence** (`crates/stack-agent/src/software_work.rs`):
  - for change work, a file change followed by a passing `pax.test` after the last change
  - for verify work, a passing `pax.test` on the unchanged project
  - **inspect** answers can be `grounded` (cite files Chip actually observed) but are never
    `verified`
- **Tests:**
  - `crates/stack-agent/tests/product.rs::a_model_that_only_claims_success_is_not_believed` and
    `a_tool_round_trip_runs_chip_over_fx_in_one_process` (grounded, not verified, exit 1)
  - `crates/stack-agent/tests/work.rs::a_model_that_only_claims_completion_is_not_believed`
  - `crates/stack-agent/tests/verify.rs::a_completion_claim_after_a_failed_result_is_refused`
  - `software_work.rs` unit tests `a_claim_of_completion_is_refused_unless_reality_supports_it` and
    `inspection_grounding_does_not_validate_claim_truth`

**Unsupported completion-evidence paths:**

- Test tampering is invisible to the runtime (D-10).
- There is no feature-acceptance predicate (D-11).
- `inspect` is never verified (D-12).
- PAX counts are partial when tests are red (D-15).

**Follow-up:** roadmap P0-01 / P1-V2.

**PAX dependency.** Verification uses an external `pax`. CI installs v0.4.1 pinned at `674f3b3`.
Several existing tests still return early (and print `SKIPPED`) when `pax`, `git` or a model is
absent (D-20). Others fail without PAX. A green run on a machine without PAX proves less than the
CI run.

## F. Credentials and transport (partially addressed)

**Credentials**

- Keys come from `CHIP_API_KEY` or `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`
  (`crates/stack-agent/src/lib.rs`, `crates/chip-fx-native/src/credentials.rs`).
- The native worker scrubs the exact key, and the proxy password, from every message it produces.
  It also truncates messages (`crates/fx-native-provider/src/provider.rs:251-271`).
- `crates/fx-core/src/redact.rs` adds pattern masking. Its generic `*_KEY=` rule is marked "not
  ported yet".

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

Not covered: crash output, anything an embedder logs itself, real providers.

**Custom root CA**

- `SSL_CERT_FILE` replaces the bundled roots (`crates/stack-agent/src/provider_selection.rs:212`).
- An unreadable or empty bundle is an error, never a silent fallback.
- Tests:
  - `provider_selection.rs::an_unreadable_ca_bundle_is_an_error_naming_the_variable_never_a_silent_fallback`
  - `crates/fx-native-provider/tests/private_ca.rs`. These need `openssl` and print `SKIPPED`
    without it, unless `FX_REQUIRE_TLS_TESTS=1`. CI sets that variable, so the tests must run.

**Proxy**

- Only an explicit CONNECT proxy is supported: `CHIP_PROXY`, `CHIP_PROXY_USERNAME` /
  `CHIP_PROXY_PASSWORD` (basic auth), and `CHIP_NO_PROXY` (`provider_selection.rs:235-290`).
- Ambient `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` are deliberately ignored.
- Tests:
  - `provider_selection.rs::chip_proxy_is_read_with_its_companions_and_ambient_proxy_variables_are_not`
  - `crates/chip-fx-native/tests/proxy.rs`
  - `crates/fx-native-provider/tests/proxy.rs`
  - `private_ca.rs::a_private_ca_endpoint_is_reached_through_a_proxy_end_to_end`
- There is no native support for the system proxy configuration.

**Insecure options.** There is no TLS verification bypass.

- Plain HTTP is allowed to loopback.
- Plain HTTP is allowed to a private-IP literal only with `CHIP_ALLOW_INSECURE_LAN_HTTP=1`, and
  then keyless (`crates/chip-fx-native/src/credentials.rs:162`).

**Gaps**

- No test against a real corporate proxy or a live TLS provider.
- No mTLS. Proxy authentication is basic only.

**Stale docs carried from chip-rs:**

- `docs/product/authoritative-fx.md` and `docs/product/production-boundary.md` still describe the
  older state (and name a removed `fx-provider-http` crate).
- `docs/product/release-limitations.md` was corrected during migration (see provenance).

## G. WASM (compile-only for the session modules)

- **CI builds these for `wasm32-unknown-unknown`:**
  - `fx-core`, `fx-single-attempt`, `fx-wasm-abi`, `fx-host`
  - `chip-core` (check only)
  - `chip-wasm-host`
  - `chip-wasm-decision`
- CI also asserts that `chip_wasm_host.wasm` has no import section (carried from chip-rs CI).
- **Executed in this repository:**
  - only `chip-wasm-decision`, under `wasmi`, in `crates/chip-wasm-decision-host` tests (which
    build the module and fail without the wasm32 target)
  - a fixture module in `chip-wasm-reasoner`
- **Not executed here:** the `fx-host` and `chip-wasm-host` modules. Their tests drive the session
  natively; the ABI is `cfg(target_arch = "wasm32")`.
- **Elsewhere:** fx's Node/V8 harnesses (`unified-runtime.yml`, `host-bridge.yml`,
  `native-provider.yml` in rkendel1/fx) ran them. That npm tooling was not migrated.
- **Unsupported:** no WASI or Wasmtime integration, no in-Wasm provider bridge. Every host/provider
  combination for the Wasm sessions is unsupported until it has a conformance test here.

**Follow-up:** add a Rust-hosted (`wasmi`) conformance run of `fx-host` and `chip-wasm-host`
before any Wasm session is called supported.

## H. Performance measurement (baseline reference only)

The available real-model measurement was supplied with the consolidation request. It is **not
reproducible from this repository**:

| | |
| --- | --- |
| Provider / model | Vercel AI Gateway, `nvidia/nemotron-3.5-lightning-free` |
| Sample size | 3 iterations |
| Inference / turn latency | min 3.63 s, median 4.12 s, max 4.42 s |
| Measured Chip overhead | min 58.5 µs, median 98.2 µs, max 107.3 µs |
| Time to first response event | not measured |
| Original report | `/tmp/chip-perf-vercel.json`, a temporary local file. It is not in this repository and was not found in chip-rs or fx |

For that one workload, Chip's orchestration overhead was small next to model latency. The
measurement does **not** isolate FX overhead or cover other providers, models, streaming paths or
`serve`. Three samples cannot establish a p95.

What this repository reproduces is the deterministic harness `crates/stack-agent/benches/baseline.rs`.
It covers sections `parts`, `loop`, `real`, `process`, `serve`, and `fx` (a scripted provider
under real FX). It asserts counts (one model call per escalation, no retries) and reports timings.
The opt-in `realmodel` and `fxlive` sections need a model, and `fxlive` reports
`time_to_first_response_event: null`. History is in `docs/product/performance.md` and
`docs/product/performance-baseline.json`, which contains no real-model rows. The harness compiles
in CI but does not run there.

**Follow-up:** a sanitized, committed `fxlive` report with time to first event and more samples,
if real-model numbers are needed.

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
