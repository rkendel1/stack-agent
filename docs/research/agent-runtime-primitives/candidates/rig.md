# Rig (deeper study)

Follow-up to the Tier 3 screen ([`tier3.md`](tier3.md#rig)). The question here is narrow: what
stack-agent's FX provider layer and chip-core work loop could learn from Rig's provider boundary,
its sans-I/O run machine, its effect records and its restore rule. Evidence tags follow
[`protocol.md`](../protocol.md). This is research only and proposes no change to stack-agent.

## Candidate record

| | |
| --- | --- |
| Repository | https://github.com/0xPlaygrounds/rig. Canonical: the workspace `Cargo.toml` and every crate's `repository` field name it ([Cargo.toml#L9][r-cargo]). |
| Inspected commit | `98ed06d7f1c911e6e4c7fdb5d690093bc964c514`, at https://github.com/0xPlaygrounds/rig/tree/98ed06d7f1c911e6e4c7fdb5d690093bc964c514 (the same clone the screen used, depth 1) |
| License | MIT text, "Copyright (c) 2024, Playgrounds Analytics Inc." (`LICENSE` at that commit) |
| Last meaningful maintenance | 2026-10-19, "chore: release v0.45.0 (#2753)", the HEAD commit. The workspace is at 0.45.0 and releases often. |
| Package used | None. No crate was built: the workspace has about 30 crates (Bevy ECS, many vector stores), and the brief rules out a full cargo build on this disk. All findings are `[source]` or `[inferred]`. |

Source links point at the pinned commit (reference list at the end). stack-agent references use
`path:line` at `rkendel1/stack-agent@366f6c13c243f8bd419e593e0bfbe67d071c9d3e`, the
[baseline](../baseline.md) commit. `git diff --stat 366f6c1 HEAD` over `fx-core`,
`fx-native-provider`, `chip-fx`, `chip-core`, `chip-agent-state` and `chip-inference` is empty at
the current worktree HEAD `b33714a`, so the lines are current.

## Summary

| Rig primitive | Already in stack-agent | New, and worth having | Port (S/M/L) |
| --- | --- | --- | --- |
| `Wire` (pure encode, per-reply decoder) | **Mostly.** `fx-native-provider::Wire` has a pure `body(&Request)` and a per-call `Decoder::feed`. FX is stricter than Rig: an unexpected frame is an error, a missing finish reason is an error, and there is delivery certainty (Rig has none) | A `Transport` seam, so recorded SSE can be replayed through the real wire and decoder. A frames-in-hand `decode_reply`. A three-way frame classification (`Known`/`Unknown`/`Corrupt`) as a diagnostic | S–M |
| Tool-call decoding | **Contract yes, wires no.** `fx-core` already defines authoritative proposals, argument integrity, identity checks and admission (`normalize.rs`). Both native wires refuse tools | Only if native tool calling is ever enabled: Rig's index/id fragment assembly for OpenAI-compatible streams and its block-stop rule for Anthropic. Its leniencies (salvaging partial objects, inferring a finish reason) must not come with it | M (only if wanted) |
| `AgentRun` (serde, sans-I/O, idempotent re-issue) | **No.** `run_work` is an async loop that holds its state in memory. Resume restarts from the goal (baseline §3.2) | A serializable work state with explicit states, a versioned envelope, idempotent `next_step`, and unforgeable answer tokens. This is the piece that would turn D2 from "partial" into "yes" | L |
| `EffectRecord` `Disposition` / `Decider` | **Partly.** FeltDB has intent → outcome and reconcile `executed`/`not_executed`. `ObservationRecord` has `status` and `executed` | Disposition vocabulary (refused before start, cancelled, interrupted, superseded) and `decided_by` (Chip rule, operator, reconcile). This would give the manual `OutcomeUncertain` settlement a recorded author | S |
| `Intercept` layers (proceed/patch/deny) | **Yes, in substance.** `validate_capability_request`, `AllowlistedBackend` and `DurableExecutor` are pre-dispatch gates and wrappers | Little. `Patch` (tool-argument rewrites) and `AfterServe::Replace` both conflict with invariants 2 and 7, and denied dispatches leave **no record** in Rig, the opposite of what Chip's audit needs | — (do not port) |
| Restore `Interrupted::{Started, Settled}` | **Yes, stricter.** An open intent without an observation gives `OutcomeUncertain` and blocks | A capability-declared *effect footprint* (`ReadOnly`/`Mutates`), so an open intent on a read-only capability can be settled as "safe to observe again" instead of blocking | S |
| Session log durability | FeltDB (see baseline §3) | Rig's compare-length append (`Changed`) is an optimistic fence at write time. FeltDB already fences with leases, so nothing new | — |

## 1. Provider boundary: `Wire` and `Transport`

### 1.1 The traits

* An `Operation` is defined once per operation, never per provider. It has associated `Request`,
  `Event`, `End`, `Response` and `Fold` types. A `Fold` absorbs events as the consumer takes them,
  and `finish(end, reply)` needs the provider's `End`: "without the provider's end there is no
  response" `[source]` [wire.rs#L176][r-op], [wire.rs#L285][r-fold].
* `Wire` is "plain data that encodes requests and names a decoder for each reply. No transport, no
  future". Implementations are `Clone + PartialEq + Debug + Serialize + Deserialize`, with
  credentials in `Secret`. It has `describe()`, `encode(request, mode) -> Payload`, `decoder()` and
  `reassembler()`. `encode` is "Pure: it may read `self`, `request` and `mode`, and nothing else"
  `[source]` [wire.rs#L743-L780][r-wire].
* `Decoder<'id, Op, Frame>` is a "synchronous state machine for one reply". `classify(frame)`
  returns `WireEvent::{Known(T), Unknown{event_type, value}, Corrupt(serde_json::Error)}`.
  `decode(event, out)` writes into the reply, and `eof(out)` defaults to
  `Err(ProviderError::Truncated)` `[source]` [wire.rs#L568][r-wireevent], [wire.rs#L713][r-decoder].
  The `'id` brand on `Out` stops a decoder from keeping its writer past one step (a `compile_fail`
  doctest) `[source]`.
* `Transport<W: Wire>` has a single method, `send(payload, exchange) -> Opening<W::Frame>`.
  "Nothing is sent until the opening is first polled"; "a failure after the reply opened is its
  last frame" `[source]` [driver.rs#L184][r-transport]. `Model<W, T>` is the pair, and "the pair
  holds no invariant, so both halves are public" `[source]` [driver.rs#L53][r-model].
* `decode_reply(wire, request, mode, frames)` decodes "frames already in hand as a live call in
  `mode` would decode them … with no transport". It is used for batch results and recorded
  streams `[source]` [wire.rs#L810][r-decode-reply].
* Driver policy on classification: `Known` is decoded. `Unknown` is warned about and delivered as
  `Item::Unknown`, and the fold never reads it. `Corrupt` is the error that ends the reply
  `[source]` [driver.rs#L608][r-triage]. A stream that runs out with the decoder still open
  (`Flow::More`) is `Truncated` `[source]` [driver.rs#L665][r-eof].

### 1.2 Anthropic and OpenAI-compatible encode/decode

* **Anthropic** `Messages` is the wire for `POST /v1/messages`, with SSE when streamed `[source]`
  [anthropic/wire.rs#L588][r-anth-wire]. Its `MessagesDecoder` keeps the open blocks and the input
  JSON streamed to each. `input_json_delta` fragments are appended to a call block. A delta for a
  block that takes no input is an error `[source]` [anthropic/streaming.rs#L353][r-anth-delta].
  * `content_block_stop` closes a block. A call whose streamed input parses to an object becomes
    complete. One whose input does not parse is kept **without a native item** (`(Kind::Call, _)
    => false` nulls the provider item, not the block) `[source]`
    [anthropic/streaming.rs#L400-L445][r-anth-stop].
  * Only a `message_delta` that carries a `stop_reason` is the provider's end. At the end, calls
    whose input is not yet JSON are closed unfinished `[source]` [anthropic/streaming.rs#L470][r-anth-end].
  * An in-band `error` event keeps the full error envelope (`from_provider_body`), and a mid-output
    model fallback is an error `[source]` (same file, `decode` and `start`).
* **OpenAI-compatible** `Chat` (`/chat/completions`) has a `ChatDecoder` `[source]`
  [openai/wire/chat.rs#L713][r-chat-wire].
  * A tool-call fragment goes to the call at its wire `index`. Without an index it goes to the open
    call with the same `id`. Without either it continues the latest call "while that call's
    arguments are not yet a whole object" `[source]` [chat.rs#L1465][r-chat-call].
  * `[DONE]` is classified as the terminal sentinel. A stream with no finish reason is `Truncated`
    `[source]` [chat.rs#L1669][r-chat-finish].
  * Dialect quirks loosen this. `done_without_finish_reason` *infers* `ToolCalls` or `Stop` at
    `[DONE]`, and `accepts_bare_string_reply` treats a bare string as the whole answer. Both are off
    by default `[source]` [chat.rs#L1680][r-chat-done], [openai/wire.rs#L317][r-quirks],
    [#L397-L403][r-quirks-default].
* **Malformed tool arguments never fail the reply.** `Out::close` documents that "a call's
  arguments are read by `ToolFunction::parse`, so malformed ones never fail the reply" `[source]`
  [operation/completion.rs#L1393][r-close]. `ToolFunction::parse` keeps the raw text in
  `invalid_arguments`, but **fills `arguments` with whatever a cut-off object still states**
  (`parse_partial_object`) `[source]` [completion/message.rs#L589][r-parse]. The run machine then
  classifies such a call as `Malformed` and never executes it (§2.3), so the salvaged object is
  display-only. It is still a repair of model output, kept in the data structure.

### 1.3 Compared with FX

| Concern | Rig | stack-agent FX | Verdict |
| --- | --- | --- | --- |
| Per-provider wire, pure request building | `Wire::encode` is pure | `fx-native-provider/src/wire.rs:25` `Wire { support, url, headers, body(&Request), decoder, secret, settings }`. `body` is pure, and `url`/`headers` are data `[source]` | covered |
| Per-reply decoder | `Decoder::{classify, decode, eof}` | `wire.rs:19` `Decoder::feed(event, data) -> Step::{Items, Done, ProviderError}`, "Strict: anything unexpected is an `Err`" `[source]` | covered, and stricter |
| Transport seam | `Transport<W>` trait, any impl | none. `Native<W>` owns the ureq exchange: `provider.rs:32`, `:304` `[source]` | **new** |
| Replay of recorded frames | `decode_reply`, plus `rig-cassette` | the `fx-core` fake operates at the `Capability` level (`fx-core/src/testing.rs`). No SSE-level replay through the real decoder `[source]` | **new** |
| Unknown frames | delivered and warned, never folded | an error `[source]` | keep FX's rule. Rig's three-way split is still useful as a *diagnostic label* for the error |
| Termination | `End` required, else `Truncated` | missing stop or finish reason is an error: `anthropic.rs:164`, `openai.rs:173` `[source]` | covered; do not adopt the quirk inference |
| Delivery certainty | none. `Opening` is lazy, but there is no "may have been sent" fact | `DeliveryCertainty`, monotonic, `may_have_executed` (`fx-core/src/contract.rs:527`) `[source]` | FX is ahead; a Transport port must keep it |
| Tool calls | decoded per provider, malformed args kept | `fx-core` `Normalizer` (`normalize.rs:1-38`): final `tool_call` is authoritative, streamed input is display-only, malformed args become `{}` with `ArgumentIntegrity::Replaced`, `admit_proposals` (`:613`) `[source]`. Native wires refuse tools (`openai.rs:54`, `:211`; `anthropic.rs:146`, `:167`) `[source]` | contract covered; wire decoding absent by design |

Stack-agent's decision contract is a JSON reply, not native tool calling, so Rig's tool-call
decoding matters only if that ever changes `[inferred]`. FX's normalizer already defines the
safer semantics: a malformed argument is replaced by `{}`, the replacement is flagged, and the
flagged proposal is never treated as valid. Rig's partial-object salvage is the counter-example
`[inferred]`.

### 1.4 Boundary sketch: an FX `Transport`

```rust
// fx-native-provider (sketch, not a proposal to merge)
/// How one request's bytes travel. The wire still owns url/headers/body/decoder.
pub trait Transport: Send + Sync + 'static {
    type Frames: FrameSource;
    /// Starts sending. Must report the delivery it can prove, as `ProviderPort::invoke` does.
    fn open(&self, req: OutboundRequest, budget: Option<AttemptBudget>, cancel: CancelFlag)
        -> Result<Self::Frames, PortFailure>;
}
pub struct OutboundRequest { pub url: String, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub trait FrameSource: Send {
    /// The next SSE frame (`event:` name, `data:`), or the end, or a transport failure.
    fn next(&mut self, wait: Duration) -> FramePoll;
    /// Monotonic, as fx_core::DeliveryCertainty::advance.
    fn delivery(&self) -> fx_core::DeliveryCertainty;
}
pub enum FramePoll { Frame { event: Option<String>, data: String }, Pending, Ended, Failed(PortFailure) }

pub struct Native<W: Wire, T: Transport = UreqTransport> { wire: Arc<W>, transport: T }
pub struct ReplayTransport { frames: Vec<(Option<String>, String)>, delivery: DeliveryCertainty }

/// Rig's `decode_reply`: recorded frames through the real decoder, no transport.
pub fn decode_frames<W: Wire>(wire: &W, frames: impl IntoIterator<Item = (Option<String>, String)>)
    -> Result<Vec<fx_single_attempt::PortItem>, String>;
```

* **Already covered:** request building, decoders, the single-attempt `ProviderPort`
  (`fx-single-attempt/src/port.rs:162`), cancellation, delivery.
* **New:** `Transport`/`FrameSource`, `ReplayTransport`, `decode_frames`. The worker loop in
  `provider.rs` moves behind `UreqTransport`.
* **Complexity:** S for `decode_frames` (the decoders are already pure). M for the `Transport`
  split, because delivery certainty and timeouts live in `provider.rs` and must stay with the
  transport.
* **Dependencies:** none new. ureq stays, behind one impl. Value: deterministic provider-wire
  tests from recorded real streams (Run 4's gemma replies, for one) without a network `[inferred]`.

## 2. `AgentRun`: the serializable sans-I/O machine

### 2.1 States and steps

* `AgentRunStep::{CallModel{prompt, history, turn}, CallTools{calls}, Done(PromptResponse)}`
  `[source]` [run/mod.rs#L76][r-step].
* Private `RunState` `[source]` [run/mod.rs#L266-L289][r-runstate]:
  * `PreparingRequest{rollback_owed}`
  * `AwaitingModel{recorded}`
  * `ResolvingToolCalls(ResolvingState)`, which scans calls and may wait on
    `resolve_invalid_tool_call`
  * `AwaitingAdvance(TurnState)`
  * `ExecutingTools(ToolBatch, TurnPolicy)`, "carrying the calls and the answers so far keeps a
    serialized run self-contained"
  * `Done`, `Failed`
* Transitions, all in `next_step` / `model_response` / `answer` [run/mod.rs#L852][r-next],
  [#L1049][r-model-response], [#L1360][r-answer]:

  ```
  PreparingRequest --next_step (turn += 1, MaxTurns check)--> AwaitingModel
  AwaitingModel    --next_step (re-issue, turn unchanged)----> AwaitingModel
  AwaitingModel    --model_response---------------------------> ResolvingToolCalls
  ResolvingToolCalls --all calls valid / resolved-------------> AwaitingAdvance
  ResolvingToolCalls --invalid call-------------> (parked; NeedsResolution to the driver)
  AwaitingAdvance  --next_step: no calls----------------------> Done
  AwaitingAdvance  --next_step: calls (assistant turn committed)-> ExecutingTools
  ExecutingTools   --next_step (re-issue: unanswered calls only)-> ExecutingTools
  ExecutingTools   --answer/answer_all completes the batch-----> PreparingRequest
  any              --protocol misuse / provider failure--------> Failed
  ```
  `[source]` (the match arms in `next_step`, `model_response` and `advance_resolution`)

### 2.2 Idempotent re-issue

* `next_step` is "idempotent while awaiting a model response or tool results: the step is
  emitted again, a model call without consuming a turn and a tool step with only its unanswered
  calls" `[source]` [run/mod.rs#L829-L852][r-idem].
* "A re-issued `CallModel` is the same attempt: it carries the same `turn`, and the run cannot
  tell its response from a late one of the attempt it replaces." The driver must drop or drain
  the previous call. Re-issues "consume no turn … and an attempt that never reached
  `model_response` … is not in `usage`. A driver that re-issues in a loop bounds it itself"
  `[source]` (same doc).
* `CallTools` re-issue is computed from `ToolBatch::pending`. `Execute` and `Malformed` slots are
  re-emitted, and `Answered` slots are not `[source]` [run/batch.rs#L40][r-pending].
* **What the machine does not know:** whether a pending `Execute` call *started*. A run persisted
  mid-`ExecutingTools` re-emits every unanswered call, including one whose effect already
  happened. Telling those apart is left to the driver (rig-ecs does it with call marks, §5)
  `[source]` (`ToolSlot` has only `Execute | Malformed | Answered`, [batch.rs#L10][r-slot]),
  `[inferred]`.
* A resume is refused only "in a pending invalid tool-call resolution" `[source]` (doc above).

### 2.3 What is serialized, and restore semantics

* All of `AgentRun` is serialized: limits, the append-only `CommittedLog` (`new_messages`), turn,
  usage, every `CompletionCall` (with raw provider JSON, ids and finish reason), retry counters,
  `previous_model`, `turn_tools` (definitions advertised per turn, "so a second driver (or a
  resumed run) can re-pair tool calls with what was offered"), host `entries` (asides) and the
  state `[source]` [run/mod.rs#L304-L352][r-agentrun].
* The envelope is versioned. `RUN_FORMAT = 5` is written on every serialization and checked on
  every deserialization (`"resume refused: the run is format {format}, this rig reads format 5"`),
  and `#[serde(deny_unknown_fields)]` refuses unknown keys `[source]` [run/mod.rs#L356][r-format].
* `CommittedLog` "only appends; the single in-place edit is the not-yet-sent prompt" `[source]`
  [run/committed.rs#L14][r-committed].
* **Answers cannot be forged.**
  * `ToolAnswer` can be built only by `ExecCall::answer` or `MalformedCall::answer`, and is keyed
    by `(turn, index, call id)` because "ids alone may repeat across turns".
  * A driver cannot construct one (a `compile_fail` doctest, E0451).
  * A `MalformedCall` cannot be answered as executed (E0308) and does not expose its arguments for
    execution (E0624).
  `[source]` [run/calls.rs#L84][r-malformed], [#L151][r-toolanswer]
* Malformed calls are classified once at batch creation:
  `None if tool_call.function.invalid_arguments.is_some() => Self::Malformed` `[source]`
  [batch.rs#L23][r-classify].
* **Restore of a started tool call (the brief's question).** The core machine does not answer a
  started call with an error. It re-emits it as pending (above). The "answered with an error
  rather than re-run" rule belongs to the rig-ecs driver's restore (§5) `[source]`. The core
  machine's own durable-execution test interrupts only *between* tool calls ("a run interrupted
  after tool call N … resumes in a fresh process image") and replays effects from an `EffectLog`
  `[source]` [rig-cassette/tests/durable_execution.rs#L1][r-durable-test].

### 2.4 Invalid-call policy

* `InvalidToolCallReason::{UnknownTool, DisallowedByToolChoice, MalformedArguments{error}}`
  `[source]` [run/policy.rs#L24][r-reason].
* `InvalidToolCallAction::{Fail, Retry{feedback}, Repair{tool_name}, Skip{reason}, Stop{reason}}`
  `[source]` [run/policy.rs#L305][r-action].
* A `MalformedCall` answered with `None` (no hook decided) "tell[s] the model to call again"
  `[source]` [calls.rs#L114-L117][r-malformed-answer].
* The defaults are `max_invalid_tool_call_retries: 0` and
  `unhandled_invalid_tool_call: UnhandledInvalidToolCall::Fail` `[source]` [run/mod.rs#L393][r-new].
* For stack-agent, only `Fail`/`Stop` are admissible (AGENTS.md §14, §21). `Retry`, `Repair` and a
  default "call again" are exactly the hidden repair loops that invariant 7 forbids `[inferred]`.

### 2.5 Compared with chip-core

| Concern | Rig `AgentRun` | chip-core `run_work*` | Verdict |
| --- | --- | --- | --- |
| Shape | sans-I/O; the driver supplies model replies and tool answers | async loop that calls `ModelProvider` and `Executor` itself (`work.rs:2166-2470`) `[source]` | **new** shape |
| Serializable state | the whole run, versioned envelope | none. Observations, decisions, `ruled_out` and evidence are in memory (baseline §3.4) `[source]` | **new** |
| Resume | deserialize, then `next_step` | `begin` → re-run from the goal with an empty `Run` (`agent-state.md:60-62`) `[source]` | **new**. Fixes D2 "partial" |
| Re-issued model call | same attempt, unbudgeted | there is no re-issue: one call per escalation, and failure ends the work `Failed{delivery}` (`work.rs:1942-1950`) `[source]` | Chip's rule is stricter. A re-issue would need FX `Delivery::DefinitelyUnsent` to be safe |
| Re-issued effect | unanswered calls re-emitted; started-or-not unknown | FeltDB intent before the effect. An open intent without an observation gives `OutcomeUncertain` (`chip-agent-state/src/lib.rs:752-795`) `[source]` | Chip is ahead. A port must gate re-issue on the intent |
| Limits | `max_turns`, separate output and invalid-call budgets | `WorkLimits{max_turns, max_executions}`, context budget (`work.rs:77-91`) `[source]` | covered |
| Decision validity | per-call classification; a malformed call is never executed | the whole decision is strict-parsed, and a malformed reply ends the work (`model_decision.rs`, `work.rs:1951-1956`) `[source]` | covered, stricter |
| Model can't mint results | `ToolAnswer` is unforgeable at the type level | the model cannot name an `ExecutionId` (`ExecutionId::unassigned`), and only `perform` records observations `[source]` | covered in substance. The type-level token is a nice reinforcement |

### 2.6 Boundary sketch: a serializable `WorkRun`

```rust
// chip-core (sketch). The pure part of run_work; the async driver shrinks to I/O.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkRun {
    #[serde(deserialize_with = "work_run_format")] format: u32, // refused unless == WORK_RUN_FORMAT
    work_id: WorkId,
    objective_digest: [u8; 32],          // resume refused if the spec changed (cf. ObjectiveMismatch)
    limits: WorkLimits,
    turn: usize,
    executions: usize,
    observations: Vec<RecordedObservation>,   // what perform() recorded; evidence is re-derived
    decisions: Vec<DecisionEntry>,            // chip-agent-state already has this type (lib.rs)
    ruled_out: Vec<RuledOut>,
    state: WorkRunState,
}
#[derive(Serialize, Deserialize)]
enum WorkRunState {
    Deciding,
    AwaitingModel { turn: usize },                      // re-issue only if FX said DefinitelyUnsent
    Executing { turn: usize, execution: ExecutionId, intent_key: String },
    Terminal(WorkOutcomeRecord),
}
pub enum WorkStep {
    Propose(WorkView),                                   // local policy (pure, sync)
    CallModel { request: ModelRequest, turn: usize },
    Execute { request: ExecutionRequest, token: ExecutionToken }, // token minted by WorkRun only
    Done(WorkOutcome),
}
impl WorkRun {
    pub fn next_step(&mut self) -> Result<WorkStep, WorkRunError>;          // idempotent per state
    pub fn decided(&mut self, d: Result<WorkDecision, DecisionError>) -> Result<(), WorkRunError>;
    pub fn observed(&mut self, t: ExecutionToken, r: Result<ExecutionResult, ExecutionError>)
        -> Result<(), WorkRunError>;
}
```

* **Restore rule (the difference from Rig).** `chip_agent_state::begin` deserializes `WorkRun`
  from the attempt's checkpoint payload. If the state is `Executing{intent_key}`, the intent's
  FeltDB status decides. Resolved means apply the recorded observation. Open with an observation
  means reconcile, then apply. Open without one means `OutcomeUncertain`: block, as today. Never
  re-emit `Execute`.
* **Already covered:** limits, decision parsing, `DecisionEntry`/`ExecutionEntry` records,
  intents, observations.
* **New:** the pure machine, the persisted loop state, the format envelope, `ExecutionToken`.
* **Complexity:** L. It splits `run_work_with_context_policy` into machine and driver, and keeps
  `verify_trajectory` and `audit_safety` deriving the same events.
* **Dependencies:** serde on `WorkDecision`/observation types; a versioned checkpoint payload in
  FeltDB `AgentCheckpoint`; the evidence store rebuilt from observations.
* **Measured value:** bench "restart" rows (baseline §3.7), where B currently redoes every step
  `[inferred]`.

## 3. `EffectRecord`, `Disposition`, `Decider`

* `EffectKind` is a serde enum: `Completion{request, stream}`, `ToolCall{name, args}`, `Embed`,
  `Rerank`, `Retrieve`, `Custom{kind, payload}`. It is routed by a string `HandlerKey` with the
  grammar `[<owner>/][<kind>:]<label>[#<generation>]` `[source]` [effect/mod.rs#L58][r-key],
  [#L631][r-kind].
* `EffectRecord{id, key, kind, outcome: Result<Answered, ErrorReport>, events?, parent?, scope?,
  purpose?, turn?, started_ms?, finished_ms?, reviewed?, disposition, decided_by?, published}`
  `[source]` [effect/mod.rs#L824-L895][r-record].
* `Disposition::{Ran (default), AnsweredBeforeStart, AnsweredWhileRunning, Superseded, Cancelled}`
  `[source]` [#L919][r-disposition].
* `Decider::{Person, Plugin(name)}`, and "an allow is not a decision" `[source]` [#L899][r-decider].
* **The record is written at resolution.** `outcome` is required, so a record exists only once the
  outcome is known. `Recorder::begin` is an in-memory hook, and `EffectLogRecorder` keeps dispatches
  in memory: "A dispatch still in flight is not in the log yet" `[source]`
  [serve/recorder.rs#L40][r-recorder], [rig-cassette/src/effect_log/recorder.rs#L29][r-cassette-rec].
  It is a replay log, not an intent log `[inferred]`.
* **Denied dispatches are not recorded.** `Recorder::discard` "removes a begun dispatch decided by a
  layer before handler execution. Replay reruns layer decisions rather than recording them"
  `[source]` [recorder.rs#L51][r-discard].

**Compared with FeltDB.** FeltDB already has intent before the action, then observation, then
outcome, and `reconcile` concludes `executed | not_executed` from the observation's `executed`
flag (`chip-agent-state/src/lib.rs:269-283`, `:752-795`) `[source]`. What Rig's vocabulary adds:

| Rig | Meaning for Chip | Today |
| --- | --- | --- |
| `AnsweredBeforeStart` | validation/authorization refused, or the executor was unavailable, before any effect | `status: "error"` with `executed: false`. The reason is in the trajectory, not the record |
| `Cancelled` | `WorkControl` stop before execution | not persisted (a cancelled execution is never retained, `lib.rs:650-668`) |
| `AnsweredWhileRunning` | an operator settled an in-flight effect | no path |
| `Superseded` | a later attempt replaced this one's intent | implicit in the `a<attempt>.` intent-key prefix |
| `decided_by: Person` | the operator who wrote the settling observation for `OutcomeUncertain` | the bench harness writes it; nothing records who (`durability_bench.rs:628-660`) |

### Boundary sketch

```rust
// chip-agent-state: chip.Observation v2 (write-once, cited by record_outcome)
#[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct ObservationRecordV2 {
    /* every v1 field: schema, task_id, intent_key, execution_id, attempt, status, executed,
       output, receipt_id */
    pub disposition: Disposition,          // Ran | RefusedBeforeStart | CancelledBeforeStart
                                           // | Interrupted | SettledByOperator
    pub decided_by: Option<Decider>,       // ChipRule(&'static str) | Operator(String) | Reconcile
}
```

* **Already covered:** intent/outcome pairing, `executed`, reconcile.
* **New:** two fields, and an operator settlement command that writes `SettledByOperator` +
  `Operator(id)` instead of a hand-written record.
* **Complexity:** S. It is a schema version bump with v1 still read, and `audit_safety` counts
  each disposition.
* **Dependencies:** FeltDB record schema (`chip.Observation`).

## 4. Policy layers (`Intercept`)

* `Intercept::before(id, kind) -> BeforeServe::{Proceed, Patch(EffectKind), Deny(ErrorReport)}`.
  A patch may not change the family or a tool call's target name, but "Tool argument patches remain
  supported" `[source]` [serve/layer.rs#L29-L45][r-before].
* `after(id, kind, outcome) -> AfterServe::{Keep, Replace(..)}`, and "the record keeps the handler's
  answer" `[source]` [#L51][r-after].
* Layers are named and listed in the handler descriptor, outermost first `[source]` [#L69][r-intercept].

**Compared with Chip.**
* Pre-dispatch refusal already exists: `validate_capability_request` runs six ordered checks
  (`chip-core/src/lib.rs:879-927`), and `AllowlistedBackend` narrows declare, availability,
  validate and execute (`capability_set.rs:114-179`) `[source]`.
* `DurableExecutor` is an `Executor` wrapper, the same pattern as a layer
  (`chip-agent-state/src/lib.rs:1135-1160`) `[source]`.

What should not be ported:
* `Patch` with argument rewrites violates invariant 7 ("do not silently alter model output").
* `Replace` makes the consumer-visible observation differ from the effect's own outcome, against
  invariant 5's "reality precedes evidence".
* `discard` removes denied dispatches from the record, while Chip's PR36 evidence *is* the record
  of rejected decisions `[inferred]`.

A named-layer list on `CapabilityDescriptor` for audit is the only small addition. It is low value.

## 5. Restore in rig-ecs: `Interrupted::{Started, Settled}`

* **Marks before effects.** "Before a call that may change something starts, the reply that asked
  for it and the call's mark are written to the store (not yet synced to disk). When they cannot
  be, no such call starts: a restart could not tell that it may have run" `[source]`
  [rig-ecs/src/stage.rs#L1100-L1130][r-stage].
  * Calls whose footprint `reads_only()` get no mark.
  * The mark is an aside `rig.call` with `CallMark::{Started{call}, Settled{call, result}}`. It is
    removed when the reply's results commit `[source]` [restore.rs#L81][r-callmark],
    [#L108][r-mark-started].
* **Restore.** "A tool call that had started or settled when the session stopped carries
  `Interrupted`." "Nothing held or approved before the restart is kept" `[source]`
  [restore.rs#L1-L12][r-restore-doc].
  * After every gate, `answer_interrupted` answers `Settled(result)` with its result, and
    `Started` with `not_finished(RESTART, true, footprint)`, decided by
    `Decider::plugin("rig-ecs")`, unless a plugin decided or holds it `[source]`
    [restore.rs#L205-L236][r-interrupted].
  * The wording depends on the footprint: read-only gives "Stopped: … It only read; run it again
    if you still need it", and otherwise "It may have partly run; check before running it again"
    `[source]` [rig-core/src/tool/feedback.rs#L201][r-not-finished].
* **Compared with FeltDB.**
  * `Settled` corresponds to "intent open, observation present", which FeltDB reconciles as
    `executed`.
  * `Started` corresponds to "intent open, no observation", where FeltDB gives `OutcomeUncertain`
    and blocks, and Rig answers the model with an error. Chip's rule fits AGENTS.md (an ambiguous
    outcome is surfaced, never guessed). Rig's rule hands an unresolved fact to a probabilistic
    judge, which invariant 2 and §4 argue against `[inferred]`.
  * Rig's footprint idea is the part worth taking. stack-agent knows "read-only" only as a
    hard-coded inspect list (`stack-agent/src/software_work.rs:1124-1144`), and
    `CapabilityDescriptor` has no effect field (`chip-core/src/lib.rs:161-174`) `[source]`.

### Boundary sketch

```rust
// chip-core: declared by the capability, never by the requester (like reuse_evidence)
pub enum EffectFootprint { ReadOnly, Mutates }
pub struct CapabilityDescriptor { /* … */ pub footprint: EffectFootprint /* default Mutates */ }

// chip-agent-state::begin, per open intent without an observation:
//   footprint == ReadOnly  -> reconcile { conclusion: "not_executed_safe_to_observe",
//                                         decided_by: ChipRule("read-only-reobserve") }
//   footprint == Mutates   -> OutcomeUncertain (unchanged)
```

* **Already covered:** blocking on uncertainty.
* **New:** the declared footprint, and one bounded, explicit recovery rule.
* **Complexity:** S. It needs a FeltDB conclusion value or a Chip-side observation, plus tests for
  the W1b and W2 bench rows.
* **Dependencies:** capability declarations in `chip-pax`, `chip-project` and `chip-compute`.

## 6. Durability D1–D4

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes.** `rig-memory` stores (`DirStore`, `MemoryStore`, `AsyncStore`), and `rig-ecs` logs every conversation change and every `Saved` component. A log append is a plain `write_all` with **no fsync**. `Writer::flush` says "No fsync: written is not yet proof against a power cut". Only `replace_log` (rewrites) syncs: temp file, `sync_all`, rename, folder sync | `[source]` [dir.rs#L259][r-append], [writer.rs#L562][r-nofsync], [dir.rs#L141][r-durable], [#L272][r-replace] |
| D2 Resume | **Yes**, at two levels. The core `AgentRun` is serde with a versioned envelope (resume = deserialize, then `next_step`). The ECS harness `restore_session` respawns agents from their logs, then reconciles once. Not run here | `[source]` §2.3, [restore.rs#L1][r-restore-doc] |
| D3 Safe recovery | **Mostly, for process crashes.** Reading is strict, and a torn last line is set aside. Appends are compare-length (`check_len` → `LogWriteError::Changed`) under a file lock, so the second of two writers is refused and stops logging. There is **no lease**: a split brain is detected at the next write, not prevented at start. A mark that is written but not synced can be lost on power loss, after which a mutating call that ran would look never-started and be re-run | `[source]` [dir.rs#L88][r-lock], [#L212][r-checklen]; `[inferred]` for power loss |
| D4 Effect safety | **Surfaced, not protected.** A started mutating call is not re-run by default: the model gets "may have partly run; check before running it again". No idempotency key reaches the tool. A model that re-issues the call duplicates the effect, now with a warning in context. The core `AgentRun` by itself re-emits unanswered calls, started or not | `[source]` §5, §2.2; `[inferred]` |

**`[blocked]`.** No E1/E2/E5 experiment. Building `rig-agent` plus `rig-core` (and `rig-ecs` with
Bevy for the restore path) was ruled out by the disk budget and the "no full cargo build" brief.
The restore claims above are from source and the module documentation. An E2 W2 against `rig-ecs`
with an `Interrupted::Started` charge remains the open experiment.

## 7. Recommendations for stack-agent (ranked)

1. **Persist the loop as data (§2.6).** This is the only Rig primitive that closes a recorded gap:
   resume from the goal, with duplicates for unobservable effects (bench: 20/20). Port the shape
   (sans-I/O, versioned envelope, idempotent `next_step`, unforgeable tokens), but gate every
   `Execute` re-issue on the FeltDB intent, which Rig does not do. L.
2. **Declared effect footprint plus one read-only re-observe rule (§5).** It reduces `stuck`
   outcomes for observations without weakening the block for mutations. S.
3. **Disposition / `decided_by` on `chip.Observation` and an operator settle command (§3).** This
   makes the manual reconciliation an attributable, recorded transition. S.
4. **FX `Transport` seam and frame replay (§1.4).** It enables wire-level regression tests from
   recorded real streams. Keep FX's strictness and delivery certainty. S–M.
5. **Do not port:** `Intercept::Patch`/`Replace`, discard-on-deny, default "call again" on
   malformed calls, partial-object salvage, finish-reason inference.

[r-cargo]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/Cargo.toml#L9
[r-op]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L176
[r-fold]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L285
[r-wire]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L743-L780
[r-wireevent]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L568
[r-decoder]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L713
[r-decode-reply]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/wire.rs#L810
[r-transport]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/driver.rs#L184
[r-model]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/driver.rs#L53
[r-triage]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/driver.rs#L608
[r-eof]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/driver.rs#L665
[r-anth-wire]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/anthropic/wire.rs#L588
[r-anth-delta]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/anthropic/streaming.rs#L353
[r-anth-stop]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/anthropic/streaming.rs#L400-L445
[r-anth-end]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/anthropic/streaming.rs#L470
[r-chat-wire]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire/chat.rs#L713
[r-chat-call]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire/chat.rs#L1465
[r-chat-finish]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire/chat.rs#L1669
[r-chat-done]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire/chat.rs#L1680
[r-quirks]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire.rs#L317
[r-quirks-default]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/providers/openai/wire.rs#L397-L403
[r-close]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/operation/completion.rs#L1393
[r-parse]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/completion/message.rs#L589
[r-step]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L76
[r-runstate]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L266-L289
[r-next]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L852
[r-idem]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L829-L852
[r-model-response]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L1049
[r-answer]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L1360
[r-agentrun]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L304-L352
[r-format]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L356
[r-new]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/mod.rs#L393
[r-committed]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/committed.rs#L14
[r-pending]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/batch.rs#L40
[r-slot]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/batch.rs#L10
[r-classify]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/batch.rs#L23
[r-malformed]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/calls.rs#L84
[r-malformed-answer]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/calls.rs#L114-L117
[r-toolanswer]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/calls.rs#L151
[r-reason]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/policy.rs#L24
[r-action]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-agent/src/run/policy.rs#L305
[r-durable-test]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-cassette/tests/durable_execution.rs#L1
[r-key]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/effect/mod.rs#L58
[r-kind]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/effect/mod.rs#L631
[r-record]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/effect/mod.rs#L824-L895
[r-disposition]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/effect/mod.rs#L919
[r-decider]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/effect/mod.rs#L899
[r-recorder]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/serve/recorder.rs#L40
[r-discard]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/serve/recorder.rs#L51
[r-cassette-rec]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-cassette/src/effect_log/recorder.rs#L29
[r-before]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/serve/layer.rs#L29-L45
[r-after]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/serve/layer.rs#L51
[r-intercept]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/serve/layer.rs#L69
[r-stage]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-ecs/src/stage.rs#L1100-L1130
[r-callmark]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-ecs/src/restore.rs#L81
[r-mark-started]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-ecs/src/restore.rs#L108
[r-restore-doc]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-ecs/src/restore.rs#L1-L12
[r-interrupted]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-ecs/src/restore.rs#L205-L236
[r-not-finished]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-core/src/tool/feedback.rs#L201
[r-append]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/dir.rs#L259
[r-nofsync]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/writer.rs#L562
[r-durable]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/dir.rs#L141
[r-replace]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/dir.rs#L272
[r-lock]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/dir.rs#L88
[r-checklen]: https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514/crates/rig-memory/src/dir.rs#L212
