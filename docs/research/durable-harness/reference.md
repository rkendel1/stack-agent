# durable-harness: where the durability guarantees actually live

Source-level, read-only investigation of `rkendel1/durable-harness` (a fork of `vercel-labs/durable-harness`, `0.0.x`) at commit `fb056a3`.
Citations are `path:line` relative to the repo root unless prefixed:

- `WSDK/world@5.0.4/...`, `WSDK/world-local@5.1.1/...`, `WSDK/world-postgres@5.0.4/...`, `WSDK/world-vercel@5.2.1/...`:
  npm tarballs fetched with `npm pack --ignore-scripts` into
  a scratch directory (not committed) (the exact versions pinned in `pnpm-workspace.yaml` catalog).
- `PI/...`: `@earendil-works/pi-durable@1.0.4` dist (same location), the version pinned in `packages/pi-world/package.json:24`.
- `LIBFX/...`: `libfx@0.0.14` package (same location).

No tests were run (the checkout has no `node_modules`). "Tests" below are test names and assertions read in source.

---

## 0. The headline: what implements "durable"

1. **durable-harness does not use the Workflow SDK's execution engine.** No `"use workflow"` / `"use step"`, no `@workflow/core` runtime, no `FatalError`/`RetryableError` anywhere in `packages/**/src` (grep). It uses only the **World interface** (`@workflow/world`: `events.create/list`, `runs.list`, `queue`, `streams`, `hooks`) as a storage/queue/stream substrate (`packages/durable-harness/src/world.ts:3-70` only resolves which World to use).
2. The replay/memoization engine is durable-harness's own **kernel**, `packages/harness-world` (bundled into `durable-harness`):
   - an "effect sandwich" (`DriveContext.effect`, `packages/harness-world/src/driver.ts:2298-2690`) that writes a born-running `step_started` (intent, with a memo key and a **replay policy** `'safe' | 'never'`) before running the body, then `step_completed` / `step_failed`;
   - a pure **fold** of the run's events into a memo table (`packages/harness-world/src/fold.ts:158-398`);
   - **recovery** of open intents by a queue delivery (`deliverOpen`, `driver.ts:580-1292`; `resolveAbandoned`, `driver.ts:1527-1571`), with liveness judged by **anchors** (`packages/harness-world/src/anchor.ts`), and single-writer enforcement by **epochs**, **first-writer-wins per key**, and (where the World offers it) the World's **in-band writer fence** (`packages/harness-world/src/session-log.ts:99-…`).
3. The recommended Pi path (`durable-harness/pi-durable`) is different again: **Pi's own durable `Harness`** (`@earendil-works/pi-durable`) supplies task checkpoints, tool intent and the replay rule; durable-harness supplies only a Pi `Storage` on a World (`packages/pi-world/src/storage.ts`) plus wake routing. Likewise fx is libfx's own durable agent (`LIBFX/durable.js`); durable-harness/fx only supplies tools.
4. CLI harnesses (Claude Code, Codex, OpenCode) run on the kernel via `packages/harness-process`, but **their model calls are not steps**; only tool calls (bridged or built-in, via hooks) and a mirror of the CLI's native session journal are in the log.

So "every model and tool call is in the run's event log" (README.md:12-16) is precise for the Pi kernel lane (`durable-harness/pi`) and fx kernel lane; it is **approximately** true for pi-durable (calls are inside Pi journal commits, not World steps; "Dashboards show journal commits (`hook_received`), not model and tool steps", `packages/pi-world/README.md:197`); and it is **not** true for CLI harnesses' model calls (`packages/harness-process/README.md:393-427`, 475-488).

Responsibility split used below: **DH** = durable-harness kernel (harness-world, pi-world, harness-process, harness-hands); **WSDK** = Workflow SDK World packages; **Backend** = storage engine (filesystem/Postgres/Vercel backend); **Adapter** = harness adapter or the harness itself (Pi, libfx, CLIs); **Sandbox** = Vercel Sandbox / just-bash worker.

---

## 1. Workflow run identity & lifecycle; session = run

**(a) Code**
- `packages/harness-world/src/sessions.ts:579-618` `start()`: mints `runId = wrun_${ulid()}`, writes `run_created` directly via `world.events.create` with `workflowName = workflow//durable-harness@<version>//<harness>` (`names.ts` `workflowNameFor`), `executionContext: { queueNamespace: 'harness' }` (`queue.ts:39`), input `{harness, config}`; then `run_started`; then `hook_created` with token `harness:<runId>:inbox`, `isWebhook: false` (comment at :607-611 explains why: avoid the SDK webhook route letting anyone post into the inbox).
- `sessions.ts:653-662` `close()`: writes `run_completed`, closes streams.
- Fold of run status: `fold.ts:166-186` (`run_created/started/completed/failed/cancelled`).
- A delivery skips any session not `running`: `driver.ts:595-598`.
- `Fatal`/too-many-restarts → `failSession` → `run_failed` (`driver.ts:1349`, `1056-1075`).
- Harness lookup by run name: `names.ts` `harnessOf` (accepts any durable-harness version and two legacy forms, so a new version drives old sessions).
- Submissions: each user message is a `hook_received` on the inbox hook (`sessions.ts:387-575` `submit`), deduped by `nonce` / `cid` in the fold (`fold.ts:230-233`); optional caller `identity` uses the World's `resumeId` dedup (`sessions.ts:478-492`).
- pi-durable: `pi-world/src/storage.ts:1003-1023` creates the run (`run_created`, `run_started`, a journal hook `pi:<run>:journal`); `worldStorage(world, id)` opens/creates.

**(b) Tests** `harness-world/src/kernel.test.ts` ("withdraws a submission that has not been placed"), `submission-identity.test.ts` ("recovers a write acknowledgment lost after commit", "recovers a lost wake acknowledgment using the same committed submission"), `names.test.ts`, `pi-world/src/harness.test.ts:68` ("answers an input, and a fresh process reads the same session back"), `:92` (queue namespace recorded on the run).

**(c) Guaranteed** One session = one World run; all durable state is that run's event log (+ run-scoped streams). A submission is durable once its `hook_received` commits; a resend with the same nonce/cid folds once.

**(d) Not guaranteed / ambiguous** Runs are created by DH, not by the Workflow runtime, so `workflow` dashboards see "steps" that are DH envelopes; the SDK's own replay semantics (determinism checks, step queue) do not apply. Run-ahead (Durable On) chat submissions are acknowledged **before** they are durable (`durable-harness/src/agent.ts:21-33`; `sessions.ts:425-472`: write failure is reported as a `submit-failed` live event and re-issued). There is no run-level cancel API in DH (see §10).

**(e)** DH (identity, lifecycle writes); WSDK/Backend (event persistence, `hook_received` resumeId dedup).

---

## 2. Event-log representation and replay

**(a) Code**
- World contract (WSDK): event ids are dense 1-based **slots**; writes carry `eventCount`; a stale write is never rejected but **bumped to the next slot and the skipped events reported** ("bump and report") — `WSDK/world@5.0.4/src/interfaces.ts:366-394`, `src/events.ts:915-945`. A `step_started` carrying input on an unknown step creates it **born-running** (synthetic `step_created`+`step_started`) — `interfaces.ts:451-460`, `events.ts:1236-1250`.
- DH effect envelope `StepEnvelope` (`fold.ts:20-51`): `{key, replay, placed?, input, epoch, retryOf?, requires?, needs?, drv, anchor, chain?, seq?}` stored as the step's input; step name `step//harness-world//<effect>` (`names.ts` `stepNameFor`).
- `SessionLog` (`session-log.ts:99-…`): one reader/writer's folded view; writes serialised with reads; folds the World's bump report so the view is always a log prefix; synthesises the born-running `step_created` (`session-log.ts:475-484`, `745-…`).
- **The fold is the only interpreter** (`fold.ts:120-157`). Rules:
  - first writer wins per memo key (`fold.ts:315-324`);
  - stale-epoch steps are fenced/inert (`fold.ts:282-289`);
  - run-ahead chain fence (`fold.ts:274-281`);
  - an outcome (`step_completed/failed/retrying`) applies only to a `running` record — late/duplicate outcomes are inert (`fold.ts:358-391`);
  - a retry is a new step naming the released one (`retryOf`), replacing it in place (`fold.ts:298-314`);
  - facts (`park/settle/detach/chaos/close/handoff/recover`) apply only if their target is in the right state at that slot (`fold.ts:405-420`).
- **Memoization**: `effect()` returns the committed `output` or rethrows the committed failure as `EffectFailure` without running `fn` (`driver.ts:2303-2305`); `isMemoized(key)`.
- **Determinism requirement** is on the harness adapter: it must request the same memo keys in the same order from the replayed state. Kernel Pi uses `model:<ordinal>` and `tool:<toolCallId>` (`packages/harness-pi/src/index.ts:262-283`, `:404`), and checks `contextLength` on replay, halting with `Restart` on mismatch (`index.ts:336-345`); fx compares tape digests and throws "fx diverged from its tape" (`packages/harness-fx/src/index.ts:503-504`, `:696`). A `Restart` rebuilds from the log; after `MAX_RESTARTS = 8` the session fails (`driver.ts:1327`, `1056-1066`). `ctx.docs` reads outside effects must not feed memo keys or inputs (`types.ts:269-276`).
- World also makes a step's first terminal outcome final (WSDK world-local: `.terminal` exclusive lock file, `EntityConflictError('Cannot modify step in terminal state')`, `WSDK/world-local@5.1.1/dist/storage/events-storage.js:1693-1740`).

**(b) Tests** `harness-world/src/fold.test.ts:47` ("fences a step written with a stale epoch, and a handoff moves the epoch at its intent"), `:175`; `born-running.test.ts` ("fold the synthetic step_created without re-reading the log"); `pipelined-log.test.ts`; `commit.test.ts:70,85` ("treats an outcome that committed before the response was lost as committed", "retries an outcome that did not land instead of losing it"); `pi-world/src/storage.test.ts` (Pi storage conformance, each case re-folded from the log).

**(c) Guaranteed** Every reader folds the same log to the same memo table; a key has at most one effective step chain; a committed outcome is replayed, never re-executed.

**(d) Not guaranteed** Determinism of the harness's key derivation is only checked heuristically (context length, fx tape digest). Payload limits are the World's (`packages/harness-process/README.md:441-453`: 50 MB step I/O, 25,000 events/run on Vercel). Reopening replays the whole log (no snapshots: `pi-world/README.md:193-194`).

**(e)** DH (fold, envelope, memo); WSDK (slot ids, bump-and-report, born-running, terminal-once); Adapter (deterministic keys).

---

## 3. Model request/response persistence

### 3.1 Kernel Pi (`durable-harness/pi`, harness-pi) and fx kernel lane
**(a)** `harness-pi/src/index.ts:262-352`: each model call is `ctx.effect('model:<n>', { name: 'model', replay: 'safe', placed, input: { appended, contextLength, model } }, …)`. Output = Pi `AssistantMessage` (whatever pi-ai puts on it, incl. usage; usage persistence not separately verified) plus optional `promptDocs`. Retries on `stopReason === 'error'` happen **inside** the effect (`modelAttempts`, default 3, `sleep(500*attempt)`) (`:300-326`). Streaming deltas go to the live stream only; the terminal event is pushed to Pi **only after the step commits** (`:309-312`) — "Pi never sees a response finish that the log does not have".
**Stable identity** = memo key `model:<ordinal>` (ordinal of model calls in the drive's replay), not a provider request id.
**Crash after provider responded, before commit**: the intent is `running`; recovery writes `step_retrying` (`resolveAbandoned`, `driver.ts:1536-1548`) → a new step with `retryOf` → **the model is called again** (re-billed; response may differ). Run-ahead (Durable On, default for Pi: `agent.ts:44`) lets a `safe` effect start before its intent is durable (`driver.ts:2336-2344`); a crash then leaves no record, and the call is simply re-issued.
**Partial streaming**: not persisted for kernel Pi (no `io.progress` on model). A cut-off stream is discarded; if the drive halts mid-stream the effect is *released* (not failed) (`driver.ts:2590-2598`).
**(b)** `harness-world/src/chaos.test.ts:360` (model step ends `completed, attempt: 2` after a crash at `model`); `harness-pi/src/pi.test.ts:345` ("keeps the transcript when a released model call is retried by another process"); `pi.test.ts:64`.

### 3.2 pi-durable (recommended Pi)
**(a)** Pi's `GenerationTask` (`PI/dist/harness/generation.js:21-…`): phases `prepare → request → (retry|poll|tools)`; each phase boundary is a durable commit (checkpoint). In `request`, partials are committed as trailing writes ~every 100 ms (`generation.js:257-300`), and on re-entry `convertPartial` appends the committed partial as an **aborted assistant entry** before re-requesting (`generation.js:95-101`, `245-255`). So a crash after the provider answered but before the classifying commit → **the model is re-called**; whatever partial was committed stays in the transcript as `stopReason: 'aborted'`. Model retry backoff "lives in the process" (`pi-world/README.md:181-183`). Each Pi commit = one `hook_received` (`pi-world/src/storage.ts:795-860`).
**(b)** `pi-world/src/harness.test.ts:68,109`; storage conformance suite (`storage.test.ts`).

### 3.3 CLI harnesses (Claude Code, Codex, OpenCode via harness-process)
**(a)** Model calls are the child CLI's; **not steps**. `ModelSteps` writes a record-only `model` step (`input {method, path}`, output `{requests, status|error}`) **only while a model-targeted crash drill is armed** (`packages/harness-process/README.md:393-427`). Recovery reads the CLI's own native session mirrored into the log by `NativeJournal` (group-committed; `harness-process/src/journal.ts:218`); spend is recorded on `hw.spend` (`ledger.ts`) with "the caps are a spend bound within that margin, not an exact one" (`ledger.ts:12-17`). "What a crash costs: The model call in flight (its partial output is not in the log; the harness regenerates it)" (`README.md:475-488`).

**(c) Guaranteed** A model response the harness acted on is in the log (kernel Pi: committed before Pi sees `done`; pi-durable: committed before visible, "Durable Strict").
**(d) Not guaranteed** No exactly-once model call: interrupted calls are re-issued (cost, nondeterminism). No provider idempotency key is used (none found). Request content is persisted only as `appended` messages for kernel Pi; full request bodies are not persisted. CLI harnesses: no model step at all outside drills; native records after the last batch can be lost.
**(e)** DH (kernel effect, commit-before-visible), Adapter (Pi/libfx/CLI own the request/response handling), WSDK (storage).

---

## 4. Tool-call lifecycle and "never run twice"

### 4.1 Classification
- Kernel: `ReplayPolicy = 'safe' | 'never'` (`harness-world/src/types.ts:9-19`); every `HostTool` declares `replay` (`types.ts:443-…`). Defaults: kernel Pi wraps tools with `tool.replay ?? 'never'` (`harness-pi/src/index.ts:406-409`); process built-ins default `never` (`harness-process/src/builtins.ts:50-52`, `replayBy`); virtual-workspace tools are `safe` (their only effect is the log-backed workspace; `harness-hands/src/runtime.ts:100,146`, `legacy.ts:127-179`); sandbox tools are `safe` for `mode: 'read'`, `never` for `mode: 'write'` (`harness-hands/src/sandbox-runtime.ts:28-31`, `:364`); demo `unsafe_send_email`/`unsafe_make_payment` are `never` (`harness-hands/src/unsafe.ts:84-105`).
- pi-durable: `tool.replay ?? "unsafe"` stored in the intent checkpoint (`PI/dist/harness/tool.js:58-64`); rerun only if **both** stored and current policy are `safe` (`tool.js:69-87`).
- libfx: every tool must declare `idempotent: true|false` or construction throws (`LIBFX/fx-harness.js:183-194`); `durable-harness/fx` virtual tools are `idempotent: true`, sandbox tools `idempotent: false` (`harness-hands/src/fx-tools.ts:650-700`).

### 4.2 Lifecycle in the kernel (`effect()`, `driver.ts:2298-2690`)
1. **Not started** = no step for the key. `effect()` checks the memo (`:2303-2305`); none → proceeds.
2. **Pre-intent phase** (optional): sandbox tools' `beforeIntent` attach; a failed/busy attach resolves the call as `NotRun` — "a definite outcome: the model may call again", never an interrupted call (`sandbox-runtime.ts:20-26`; `harness-pi/src/index.ts:391-402`).
3. **Intent** = born-running `step_started` with the envelope (`:2398-2435`). For a `never` effect this is a **barrier**: under run-ahead, everything issued is durable before its body starts (`:2336-2344`); a CLI built-in `never` call also waits for the native journal to flush first (`builtins.ts:139-144`, `beforeNever`). If the intent lost first-writer-wins or was fenced → `step_failed {message:'superseded'}` and `Yield('contended')` / preempt; **the body never runs** (`:2436-2467`). With the World's in-band fence, a stale drive's intent is refused by the World and "its effect never starts" (`:2424-2430`).
4. **Body** `fn(io)` (`:2524`), with `io.progress()` streamed to `<runId>_<stepId>` (kept as interruption evidence).
5. **Outcome**: `step_completed` (output + any doc edits atomically, `:2629-2677`) or `step_failed` (`:2600-2620`). Outcome writes that throw are looked up in the log before being reported, and retried ×3 (`commit`, `driver.ts:1797-1870`): "acting on a failure that did not happen would hand a `replay: 'never'` effect's caller an error for work that ran". If a recovery resolved the intent first, the drive gets `Restart` and rebuilds from the log — **the harness never acts on a result the log does not have** (`:1787-1796`).

### 4.3 What happens to an in-flight call after a crash — exact code path and text
Recovery (`deliverOpen` step 1-2, `driver.ts:646-835`) finds `running` records, judges each anchor (§6), and calls `resolveAbandoned` (`driver.ts:1527-1571`):
- `safe` → `step_retrying` with `{message: 'interrupted: the process running this effect stopped', interrupted: true}` → record goes `pending`; the harness's next `effect(key)` writes a new step with `retryOf` and **reruns the body** (`fold.ts:298-314`).
- `never` → `step_failed` with
  `'interrupted: the process running this effect stopped before it reported a result, and it is not safe to rerun. It may or may not have taken effect.'`, `interrupted: true`, `partial: <the step's progress stream>`; **never rerun**; on replay `effect()` throws `EffectFailure` with this error (`driver.ts:2305`).
- A drive that stops itself mid-effect (budget, preemption) releases the same way via `release()` (`driver.ts:2187-2211`), message `interrupted: the drive stopped (<why>) before this effect reported a result, and it is not safe to rerun. It may or may not have taken effect.`
- Work cut off from outside (VM died) → `EffectInterrupted` → same interrupted shape (`errors.ts:46-58`, `driver.ts:2599-2606`).
- Parked work (detached sandbox command) whose settlement is "lost": `safe` re-runs up to `MAX_PARKED_RETRIES = 3` (`driver.ts:1574`, `2094-2104`); otherwise the failure is committed.

**What the model is told**
- Kernel Pi: the tool result is an error whose text is the message above plus `\nPartial output before the interruption:\n<last progress chunk JSON>` (`harness-pi/src/index.ts:549-560`).
- Process harnesses (bridged tools): `failureText` = same message + partial (`harness-process/src/tools.ts:140-152`). On resume, the turn starts with a **recovery note** user message built from the log (`harness-process/src/recovery.ts:58-115`):
  `[Recovery] Your previous turn was interrupted: the process running it stopped, and this session was resumed from its durable log.` then per call: `completed. Its result:` / `failed: <err>` / `interrupted before it reported a result. It is not safe to repeat, and it may have taken effect: check before repeating it.` (never) / `interrupted before it reported a result. It is safe to repeat.` (safe) / `interrupted, and it may still be running. It may have taken effect: check before repeating it.` (still `running`); or `No tool call had started in that turn.`; closing line `Check the actual state before repeating anything that may have taken effect, then carry on with the task.` Built-ins: "Nothing reruns a built-in call by itself … For a built-in, `safe` only changes the note's wording: the model decides what to repeat" (`builtins.ts:30-37`). A CLI that exits mid-call → `interruptAll` commits `the harness stopped before this call reported its result: it may or may not have taken effect` (`builtins.ts:228-236`).
- pi-durable: `Tool <name> was interrupted and may have partially run` (status `interrupted`) (`PI/dist/harness/tool.js:83-86`); unstarted calls of an aborted generation get `Tool <name> was aborted` (`generation.js:183-193`).
- libfx: "the model is told it may have partly run, and the turn goes on" (`LIBFX/durable.js:1715-1719`; text itself not extracted — unverified wording).

### 4.4 STARTED-but-not-COMPLETED vs never-started
- **Yes, DH distinguishes "intent committed" from "no intent"**, and that is the only distinction it can make: the log state for the key is either absent (never started → the call runs normally when the harness re-derives it; for process harnesses the note says "No tool call had started in that turn" when none was recorded), `running` (open intent), `pending` (released safe retry), `failed{interrupted}` or `completed`.
- **It does NOT distinguish "intent committed but body never began" from "body began/partly ran".** Both are reported "may or may not have taken effect". The only extra evidence is `partial` (whatever the body reported via `io.progress` before dying) — e.g. the unsafe demo tool reports `{sideEffect: ref, phase: 'confirming'}` right after its side effect (`unsafe.ts:94-95`), so the model sees the side-effect id.
- A distinct third outcome exists only before the intent: `NotRun` from the pre-intent attach phase (definite, may retry).
- pi-durable makes the same cut: phase `call` (pre-intent: resolve/validate/`beforeTool`, re-run freely) vs phase `execute` (after the intent commit at `tool.js:58-64`).

**(b) Tests** `harness-hands/src/retry.test.ts:183` ("a crash in the middle of an unsafe tool, after its side effect … is not rerun: one ledger record, and an interrupted result"; asserts `error.message` matches `/not safe to rerun/`, ledger length 1) and `:134-160` (safe workspace tools: "the retry produces exactly one clean run's result"); `harness-world/src/kernel.test.ts:99` ("recovers a crashed invocation with the effect replay policy": `interrupted: true, partial: [{started:'HELLO'}]`, model step `attempt` stays 1); `anchor.test.ts:265` ("never hands the harness a <replay> result the log rejected in favour of a recovery's"); `runahead-blips.test.ts:49,101,178`; `in-band-fence-processes.test.ts:22` ("two processes race one turn on one data directory: one is refused, the tool runs once"); `harness-pi/src/pi.test.ts:294`; `harness-process/reference/reference.test.ts:125`; `harness-claude-code/test/harness.test.ts:186,199,503,564`, `sandbox-restart.test.ts:381,405,643`; `harness-pi/test/sandbox.test.ts:424`; `pi-world/src/harness.test.ts:109` (`ran = {safe: 1, unsafe: 0}`, transcript contains `interrupted`).

**(c) Guaranteed** A `never` effect's body is started at most once **per memo key by any drive that the log accepts as owner**, because (i) its intent is durable before its body, (ii) the key is first-writer-wins, (iii) recovery converts an open `never` intent to a terminal failure, never a retry, (iv) a stale drive's intent is refused (in-band fence) or inert (epoch/first-writer).

**(d) NOT guaranteed**
- **"Never run twice" ≠ "ran once"**: a `never` call may have run zero, partial, or one time; the model/user is told "may or may not have taken effect". No reconciliation/idempotency key to the external system is provided (demo tools write their own ledger).
- The guarantee is only as good as the **replay classification**. A tool mislabelled `safe` will be re-run. Kernel Pi defaults to `never`; pi-durable to `unsafe`; but a `HostTool` must declare `replay` itself.
- Built-in CLI tools: the kernel can't stop the CLI from re-dispatching; it returns the log's answer from `begin` and relies on the harness not running it ("must not run it", `builtins.ts:34-36`). Built-in commands are killed with the drive "so it may have partly run" (`README.md:483-484`).
- **A frozen/zombie holder** taken over is fenced from *writing*, but its body may still be executing: "a non-safe tool it was running may have run" (`pi-world/README.md:186-190`); the kernel test only shows the thawed tool observing `io.signal` abort (`anchor.test.ts:311`). The body's process is not killed by the fence.
- Live stream writes after a takeover are not fenced (`docs/followups.md:32-41`).
- Concurrent tool calls: "the bridge serializes; a harness's own parallel calls … overlap as effects" (`harness-process/README.md:497-498`).

**(e)** DH kernel (intent/outcome, recovery, messages); Adapter (classification, determinism, telling the model); WSDK/Backend (terminal-once, fence); Sandbox (`FSYNC` before outcome, VM kill semantics).

---

## 5. Recovery of interrupted turns; checkpoint/continuation boundaries

**(a)** `deliver` → `deliverOpen` (`driver.ts:342-424`, `580-1292`): (0) companion stand-down; (1) classify open intents via `anchorVerdict` (`:646-680`): half-written facts are finished (`finishFact`), `covered` work → ack and leave it, `gone`/`mine` → recover; (2) recovery: if fencing is needed, commit an epoch-moving `recover` step (`commitEpochStep`, `:767-796`), then `resolveAbandoned` each, detach orphaned parked steps (`:797-812`); abort if the epoch moved meanwhile (`:820-828`); (3) drive the harness from the fold. Boundaries = each effect's intent and outcome; between effects a turn resumes at the next un-memoized key. Budget exhaustion → `Yield('budget')` → a **continuation** queue message that is granted the drive's anchor (`continuation`, `:892-905`; `continues` in `DeliverMeta`). `workRemains` (`driver.ts:435-460`) decides if a wake has anything to do. Process harnesses add the recovery note and `restartEvent` (`harness-process/src/recovery.ts:136-139`). World-restart recovery: WSDK `reenqueueActiveRuns` uses `executionContext.queueNamespace`; DH also has `recoverSessions` for shared data dirs (`harness-world/src/recover.ts:17-59`).
pi-world: a commit is a Pi checkpoint; `wakeHandler` opens, `resume()`, waits `untilSettled`, closes; budget → continuation (`pi-world/README.md:66-88`).

**(b)** `chaos.test.ts:39,79,110,148,183,295,360`; `budget-boundary.test.ts:60,93,179`; `anchor.test.ts:127,207,217,343`; `grants.test.ts:162`; `drive-route.test.ts:100,159,171`; `pi-world/src/harness.test.ts:211,242,260,278`; `durable-harness/test/agent-runs-recovery.test.ts`.

**(c)** A turn interrupted anywhere resumes at the first effect without a committed outcome, in any process that can read the log and receive the queue message.
**(d)** In-flight (uncommitted) model output and native CLI records since the last batch are lost; recovery latency depends on the anchor (seconds on a lease lapse; ~65-70 s for a silent direct-drive death, `docs/queue-send-and-hold.md:36-38`; on world-local, next process start; on multi-host Postgres see §6). Divergent replay ends in `run_failed` after 8 restarts.
**(e)** DH; WSDK queue for redelivery.

---

## 6. Execution ownership & concurrency

**(a) Layers**
1. **In-process**: one drive per session per process (`claimDrive`, `anchor.ts:286-300`; `deliver`, `driver.ts:357-385`), reruns capped at 8 then re-queued.
2. **Anchors (liveness)** `anchor.ts:7-41`: "Liveness is the queue's, never ours … no beats, probes, leases or idle lines." Every intent records its anchor (`msg` = queue message id; `deadline`; `tab`; `host`). `anchorVerdict` (`anchor.ts:514-533`) + `judgeAnchor` (`:535-545`): `mine` (own message redelivered or a continuation grant) → recover fenced; `gone` (drive `close`d, epoch moved, a chaos crash record names its drive, deadline passed, or the World policy proves it: dead/reused pid on same host `localAnchors` `:362-385`, other message on an exclusive World `:394-402`, free Web Lock `:416-472`) → recover; else `covered` → ack.
3. **Epoch fencing** (DH): `recover`/`handoff` steps increment `state.epoch` at their intent (`fold.ts:328-330`); any later step written with an older epoch is inert for every reader (`fold.ts:282-289`); drives watch hints and refresh every 15 s to stop on preemption (`driver.ts:860-876`, `PREEMPTION_REFRESH_MS`).
4. **World in-band writer fence** (WSDK + Backend): `CreateEventParams.inBand/expectedSeqInBand`, refused with `InBandSupersededError` (HTTP 412) writing nothing (`WSDK/world@5.0.4/src/events.ts:793-844`, `interfaces.ts:609-644`); note "The runtime does not set it yet" — DH is the user. Implementations: world-local per-run count file under `proper-lockfile` + in-process lock (`WSDK/world-local@5.1.1/dist/storage/events-storage.js:385-407`, `2734-2850`); world-postgres `SELECT … FOR UPDATE` on `workflow_event_slots` + CTE `UPDATE … WHERE seq_in_band = $expected` in one transaction (`WSDK/world-postgres@5.0.4/dist/storage.js:153-215`, `800-845`); world-vercel declares it (`dist/index.js:69`) but lists no snapshot, so DH obtains the count by a deliberately refused **probe** write (`session-log.ts:198-…`). DH arms it per drive (`SessionLog.arm`, `session-log.ts:162-185`; `deliverOpen` `:743-745`); a refused drive yields `superseded` (`driver.ts:1717-1724`) and the delivery re-decides up to `MAX_FENCE_REFUSALS = 3` (`:578`, `747-760`). After a write with unknown outcome, the drive is "no longer fenced" and continues out-of-band (`types.ts:322-345`).
5. pi-world: `after` chain — "a journal entry counts only when it was written on top of the newest entry that counted" — plus the in-band fence; a loser gets `StorageFenced` and Pi poisons the session (`pi-world/src/storage.ts:293-350`, `795-860`).
6. **Send-and-hold** (`docs/queue-send-and-hold.md`): today a non-queue drive (`chatRoute` default `drive: 'route'`) runs under a `deadline` anchor (`directDriveMs` default 60 s, `driver.ts:215`) with a **companion** queue message delayed `ceil(deadline-now)+5` s; no effect starts in the last 15 s (`deadlineMarginMs`); an effect still running at the deadline is cut and resolved by replay policy. Send-and-hold (a message born leased to its sender) is **not implemented** — "Status: in progress in Vercel Queues and the Workflow SDK's World" (`queue-send-and-hold.md:94-96`); workaround `chatRoute({ drive: 'queue' })`.

**(b)** `in-band-fence.test.ts:470,519,875,912` (incl. "two drives on two World instances (two hosts on one store) race the same turn: one of them is refused"); `in-band-fence-processes.test.ts:22`; `drive-fence.test.ts:113-278`; `verdicts.test.ts:34,44,57,93,148`; `anchor.test.ts:150,168,311`; `heartbeat-free.test.ts:56,90,99`; `hold-anchor.test.ts`; `pi-world/src/fence.test.ts`, `harness.test.ts:160`; `harness-world/src/queue-postgres.test.ts` (exists; not read in detail).

**(c)** Two workers can both *run* a drive concurrently, but at most one's writes take effect: the World fence refuses the stale one's first in-band write (where supported), and independent of that, epoch + first-writer-wins make stale writes inert in the fold. A `never` intent from the stale drive is refused/inert before its body runs.
**(d)** Not a mutual-exclusion lock on *execution*: a stale/frozen holder can keep executing bodies until it notices (hint, 15 s refresh, its next write). `localAnchors`: "A foreign host is never judged" (`anchor.ts:356-361`) and for world-postgres "Across hosts, only a restart redelivers" (`anchor.ts:387`) — so on multi-host Postgres a dead host's open work is `covered` until the queue redelivers (exact graphile-worker redelivery timing: unverified). Deadline anchors compare wall clocks across machines ("server clock", `anchor.ts:54`); skew effects are mitigated by fencing but not analysed in code. Live stream writes are not fenced (`docs/followups.md:32-41`). On a World without the fence (older World, `HARNESS_IN_BAND_FENCE=0`) a stale intent lands and the fold must make it inert (`in-band-fence.test.ts:519`).
**(e)** DH (anchors, epochs, first-writer, arming); WSDK (fence API, queue redelivery/leases); Backend (atomic count check); queue provider (VQS lease renewal by `@vercel/queue`).

---

## 7. Retry behavior & failure classification

**(a)** No Workflow SDK `FatalError`/`RetryableError`/step retries are used. DH's own classes (`harness-world/src/errors.ts`): `EffectFailure` (committed failure, rethrown on replay), `Yield(budget|contended|handoff|preempted|parked|superseded)`, `EffectInterrupted`, `Parked`, `Restart`, `Fatal` (fails the run). Retry knobs: replay policy (`safe` re-run after interruption, unbounded except parked: `MAX_PARKED_RETRIES = 3`), in-effect model retries (`modelAttempts` 3, harness-pi), outcome write retries ×3 with lookup (`driver.ts:1823-1862`), `MAX_RESTARTS = 8` → `run_failed`, `MAX_FENCE_REFUSALS = 3`, rerun cap 8, `maxFailedDeliveries` default 8 → the turn fails with code `failing` (session stays running) unless the error is a transport/World error (`unavailable.ts:135-151`); `Unavailable` (harness cannot run here) fails the turn, not the run (`driver.ts:1080-1100`, `unavailable.ts`). Queue redelivery/backoff is the World's (world-local 5 s linear backoff, `WSDK/world-local@5.1.1/dist/queue.js:302`).
**(b)** `unavailable.test.ts`, `registry.test.ts:100`, `commit.test.ts`, `lanes.test.ts:203` ("releases a safe effect whose work stops the drive, instead of recording a failure").
**(c)/(d)** A `safe` effect interrupted by repeated crashes is retried with no cap other than redelivery limits (only parked work is capped) — potential crash loop on a poisonous safe call (unverified whether something else bounds it). Tool errors are committed outcomes (no retry).
**(e)** DH; WSDK queue.

---

## 8. Durable workspace / virtual filesystem and sandbox state

**(a)** Documents (`harness-world/src/docs.ts:1-30`): small JSON state per session in the log; **in-band edits ride on an effect's `step_completed` envelope `{ $docs: 1, value, docs }`, atomic with it**; a call that throws or never reports changed nothing; out-of-band app edits via a docs hook with optimistic concurrency. Max 64 KB per doc (`DOC_MAX_BYTES`). Workspace = `hw.workspace` manifest doc (`harness-hands/src/files.ts`, `doc-workspace.ts:31-60`): just-bash in an isolated worker per drive, synced to the manifest; files > 4 KB are blobs in the session's blob stream (hash-checked) named by the manifest; one call at a time; a failed call is rolled back; a worker that died loses its in-memory FS and is rebuilt from the manifest. Hence workspace tools are `safe`: a retry starts from the pre-call files. pi-durable: `bindTool` puts changes in the tool result's `details` (`hw.workspace`), folded on open (`pi-world/README.md:111-123`, `pi-world/src/files.ts`). fx: per-call records keyed by libfx `executionId` (`durable-harness/src/fx.ts:27-35`).
Sandbox (`sandbox()`): files on the session's Vercel Sandbox Drive at `/workspace`, "Durable, not transactional": writes survive a crash, so write tools are `never`; each write `FSYNC`s before the outcome commits; Drive loss is detected and the model told (files-lost note) (`harness-hands/src/sandbox-runtime.ts:1-45`); the sandbox relay is armed with the drive's fence token so an older drive's commands never win (`types.ts:276-310` `DriveFence`). CLI built-ins: working dir is a disk, not the log (`harness-process/README.md:486-488`, "A built-in's working directory in the log" listed as not done).
**(b)** `harness-hands/src/retry.test.ts:134-180`, `doc-workspace.test.ts`, `doc-workspace-adversarial.test.ts`, `virtual-workspace.test.ts`, `harness-world/src/docs*.test.ts`, `harness-pi/test/sandbox.test.ts:281,424`, `harness-claude-code/test/sandbox-restart.test.ts:381-1105`, `packages/harness-fx/test/sandbox.test.ts`.
**(c)** Virtual workspace state is exactly the fold of committed outcomes; no partial edit of an interrupted call is ever visible.
**(d)** Orphan blobs from interrupted calls remain (`retry.test.ts:162`). Virtual workspace has no network/real processes. Sandbox and CLI built-in disks are not transactional with the log. Spec doc `docs/workspaces.md` is referenced ~179 times in code but **absent** from this checkout.
**(e)** DH (docs, manifest, blobs); Sandbox (Drive durability, VM); WSDK (streams for blobs).

---

## 9. Streaming & reconnection

**(a)** Live UI events go to the World stream `<runId>_live` via an ordered, coalescing `JsonLinesWriter`; "Publish a UI event … Best effort" (`types.ts:207`, `streams.ts:56-…`). Resumable tails with opaque cursors `<startIndex>.<lines>` usable as SSE `id` / `Last-Event-ID` (`streams.ts:258-350`; `live-response.ts:18-90`, `chatRoute` `:251-320`). On recovery, a `restart` live event tells clients to replace the turn's text with what the log kept (`harness-process/src/recovery.ts:125-139`); run-ahead rewinds emit `rewind` events (`driver.ts:1741-1775`). Per-effect progress streams double as interruption evidence. Stream reconnect on cut reads (`streams.test.ts:94` "re-opens a cut read and delivers nothing twice"; `docs-adversarial.test.ts:154`).
**(c)/(d)** The live stream is not authoritative and not fenced; deltas from a stalled drive can interleave after a takeover (`docs/followups.md:32-41`). Authoritative results are read from the log (e.g. `harness-fx/src/durable.ts:8-15`).
**(e)** DH; WSDK streams.

---

## 10. Cancellation & timeouts

**(a)** Timeouts: invocation budget = min(World runtime deadline, `maxInvocationMs` default 10 min, anchor deadline) minus `deadlineMarginMs` 15 s (`driver.ts:1504-1517`, `841-850`); budget abort → `Yield('budget')` → continuation; watchdog gives the delivery up on write stall (`WRITE_STALL_MS` 30 s), run-ahead stall, or past the hard deadline, writing nothing (`driver.ts:1244-1273`). Direct drive 60 s (`directDriveMs`). pi-durable storage backstop `recoverAfterMs` 120 s, `maxDriveMs` 10 min (`docs/queue-send-and-hold.md:60-63`, `pi-world/README.md:181-183`).
Cancellation: only **withdraw** of a not-yet-placed submission (`sessions.ts:668-675`; fold `:244-253`) and `close` (`run_completed`). The fold honours `run_cancelled` (`fold.ts:184-186`) so a World-level cancel stops future drives, but DH writes no cancel and exposes none in `chatRoute` (kinds `prompt|steer|followUp` only, `pi-durable.ts:583`). libfx-shaped handle: "Once a turn is running it runs to completion somewhere; cancel only stops watching" (`harness-fx/src/durable.ts:40-44`). Pi has its own task `abort` phases (`PI/tool.js:89-93`, `generation.js:168-197`) — reachability from durable-harness routes: unverified.
**(d)** No mid-turn user cancel in the kernel; a cut effect relies on the body honouring `io.signal`.
**(e)** DH; Adapter (Pi abort).

---

## 11. Observability (Agent Runs)

**(a)** `durable-harness/src/instrumentation/drive.ts` (`traceDrive`), `checkpoints.ts` (`TraceStreams`: "Best-effort checkpoints: bounded I/O"), `otel.ts`; `harness-world/src/agent-runs.ts` (`DriveTracing`). Traces are **derived** from effects (or `ctx.tracing.modelCall` for `modelCalls: 'self'` harnesses), with trace checkpoints in a separate run stream so a turn spanning drives stays one span; "work already in the log is never traced again". pi-world: "Telemetry never rejects a durable commit … Replaying a journal emits no spans" (`pi-world/src/storage.ts:851-866`). Failures degrade to untraced (`agent-runs-recovery.test.ts:154,166,312`).
**(c)/(d)** Not authoritative: best effort, may be missing or partial; the event log is the source of truth. Trace content gated by `traceAudience`.
**(e)** DH + `@vercel/agent-runs`.

---

## 12. Storage backends, transactions, deployment assumptions

- **world-local** (filesystem): slot allocation per publish, exclusive lock files for terminal step transitions, in-band count file under `proper-lockfile` (cross-process on one data dir). Queue is **in memory**: messages die with the process; recovery happens on the next process start (`recoverActiveRuns`/`reenqueueActiveRuns`, DH `recoverSessions`), and anchors judge the dead pid (`anchor.ts:356-385`, `drive-route.test.ts:171`).
- **world-postgres**: one transaction per fenced create (`FOR UPDATE` on the run's slots row + CTE), graphile-worker queue; per DH comment, the job lock "is never renewed, so only a process restart redelivers" and foreign hosts are never judged (`anchor.ts:354-361,387`) — cross-host failover timing unverified.
- **world-vercel**: VQS lease renewed by `@vercel/queue` while the handler runs; redelivery after lease lapse proves loss (`anchor.ts:345-353`); declares the fence but lists no snapshot (probe path). Vercel limits per `harness-process/README.md:441-453`. `withDurableHarness` writes queue triggers `__harness_wkf_workflow_*` with `maxDuration: 'max'` (`durable-harness/src/next.ts:380`).
- **world-remote** (`packages/world-remote`): storage/streams over HTTP on the server, **queue local to the client** (tab), run-scoped capability tokens (`run-scope.ts:1-40`); liveness of tab-driven lanes via Web Locks / tab-lost callbacks.
- Transaction boundary in DH = **one event**: intent and outcome are separate events ("A record-only effect is two events", `harness-process/README.md:492-493`); doc edits are atomic only because they ride inside one outcome event.

---

## 13. Explicit limitations / follow-ups documented in the repo

- `docs/followups.md`: four libfx 0.0.14 tests skipped (kernel-lane steer placement diverges on replay; "P0.12 across two processes"); **live writes after a takeover are not fenced**.
- `docs/queue-send-and-hold.md`: 60 s direct-drive limit; long `never` commands near the deadline are cut and reported interrupted; send-and-hold not shipped; pi-durable exposure when one command runs past `recoverAfterMs` with nothing committed.
- `packages/pi-world/README.md:176-197`: Durable Strict only; model retry backoff in process; takeover is the queue's and a frozen holder's non-safe tool may have run; whole-journal replay, no snapshots; dashboards show commits, not steps.
- `packages/harness-process/README.md:475-503`: crash costs (in-flight model output, built-in may have partly run, native records since last batch, working dir if the machine goes); not done: batched record-only effects, model drills without the proxy, concurrent tool calls, working dir in the log, `flock`, Sandbox lane for process harnesses.
- `packages/harness-process/src/ledger.ts:12-17`: spend caps approximate.
- README note: experimental `0.0.x`.
- `it.todo`s in `packages/harness-sandbox/test/relay.test.ts:625,654,681`.
- `docs/workspaces.md` is cited throughout the code but missing from the checkout (unverified content).

## 14. Unverified items

- Exact libfx interrupted-call wording; whether Pi abort is reachable from DH routes; postgres cross-host redelivery timing; whether model `usage` is always present in kernel-Pi model outputs (depends on pi-ai `AssistantMessage`); whether any bound exists on repeated re-runs of a crashing `safe` effect beyond queue redelivery behaviour; `queue-postgres.test.ts` contents; behaviour of world-vercel's backend fence (only its declaration and DH's probe path were read).
