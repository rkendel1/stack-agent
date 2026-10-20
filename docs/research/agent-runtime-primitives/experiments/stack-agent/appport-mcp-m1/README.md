# AppPort over MCP M1: Stack-Agent consumes an existing campaign bridge

**Question.** Can Stack-Agent's work loop consume an existing AppPort campaign bridge through MCP, with
the application staying the authority? Specifically: discover the tools through MCP, without
capability-specific tool definitions in Chip; satisfy their input contracts; invoke a real operation
under the configured authority; use the real result to answer a task; tell successes, application
errors and authorization refusals apart; and produce evidence that independently agrees with the
application.

**Answer: yes, in replay, for reads and for one scoped write.** Two real models (through the AI
Gateway) answered from Vikunja's data, made the scoped write, met Vikunja's refusals and errors, and
were refused when they claimed answers they had not observed. Vikunja's own tables back every claim
Chip accepted. The run is a replay: the MCP calls were made for real, by a recorder outside Chip,
and served to the loop from that recording. No MCP server is called from inside a run (a live mode
is new authority under `AGENTS.md` §14). The remaining limits are in section 6.

Labels: **measured** (run here), **read** (from code).

This record tests integration only. It does not test generation (`appport add`); that is
`../appport-generation-g1/` and, with the patched generator, `../appport-generation-g2/`.

## 1. Target and setup (measured)

| Part | What was used |
| --- | --- |
| Bridge | `rkendel1/vikunja` branch `appport/bridge` (PR #1) at `851a9da`, `appport-bridge/`, **unchanged**, installed from its own `package-lock.json` (`npm ci`) |
| Bridge packages | its own pins: `@appport/sdk` 1.2.2, and the `@appport/mcp` 1.3.0 that SDK depends on (`@modelcontextprotocol/sdk` 1.32.1). No package was added |
| Application | Vikunja built from the same fork commit (`go build -tags osusergo`, stub frontend), SQLite in a temp dir, disposable |
| Baseline | the bridge's own consumer test against that build: **13/13 pass** |
| MCP host | `crates/chip-appport-source/recorder/record-mcp.mjs`: `createApplication` + `permissionAuthorizer` from the bridge's SDK over the capabilities `bridge.mjs` exports, with the application and resources the unchanged bridge process serves in its manifest; `createMcpHandler` from the bridge's `@appport/mcp`; Streamable HTTP on loopback, operator bearer required |
| Authority | the MCP session's AppPort permissions are the **operator's grant**; the Vikunja API token the bridge forwards is the operator's configuration (never recorded). Vikunja decides every access with that token |

Each phase is one operator configuration:

| Phase | AppPort grant | Vikunja token (bob) |
| --- | --- | --- |
| `read` | `vikunja.read` | projects `read_all`/`read_one`, tasks `read_one` |
| `write` | `vikunja.read`, `vikunja.tasks.write` | the same plus tasks `update` |
| `write-token-read-only` | `vikunja.read`, `vikunja.tasks.write` | the read-only token |

Data (seeded by the operator through Vikunja's v1 API): bob owns `Garden` (task 1 *Water the
turnips*, open; task 2, done) and `Kitchen` (task 3); carol owns `Studio` (task 4).

The recorder writes every JSON-RPC request and response. It also reads Vikunja's `tasks` and
`projects` tables directly, read-only, **right after every call** and at the end of each phase:
`fixtures/mcp/vikunja.{read,write,write-token-read-only}.mcp.json`.

## 2. Discovery and the input contract (measured, read)

`McpSource` (`src/mcp.rs`) builds every descriptor from `tools/list`. The crate contains no tool name,
input or schema.

* `tools/list` returns 3 tools in every phase. A tool is offered only when its
  `_meta["dev.appport/capability"]` names the capability and its required scopes, and those scopes
  are within the grant:
  * under `read`, `vikunja_tasks_setdone` is listed but not offered (it requires
    `["vikunja.tasks.write"]`, which the grant does not hold);
  * a tool without that metadata is not offered, because its authority cannot be read.
* Ids are `appport.vikunja.<tool>`. Inputs and required inputs come from `inputSchema`, with the JSON
  type in each description (`id`: required, "JSON integer").
* The experiment's allowlist is `source.ids()`, the grant's ceiling, so the test names no
  capability either.
* The bridge declares `effect: "observation"` for the reads. Chip reports that label and still treats
  every tool as `Consequential` and `Unconfined`, so the source is offered in replay only.
* Detection needs `appport.binding.json` (`transport: "mcp"`) naming the server that answered
  `initialize` (`Vikunja` / `appport-bridge-1`).

## 3. Outcomes as MCP returns them (measured)

| Call | MCP result | Chip's classification |
| --- | --- | --- |
| `tasks.get {id:1}` | result + `structuredContent` | success |
| `tasks.get {id:4}` (carol's) | `isError`, `FORBIDDEN` / `forbidden`, "the application denied access" | **refused**, by Vikunja |
| `tasks.get {id:999999}` | `NOT_FOUND` / `not_found` | failed (application error) |
| `tasks.get {id:"1"}`, `{}` | `INVALID_INPUT` with issues (the bridge's contract; Vikunja not called) | failed |
| `setdone` under `read` | `FORBIDDEN`, "Missing permission: vikunja.tasks.write" (AppPort's authorizer) | never offered, so never replayed |
| wrong MCP bearer | `UNAUTHORIZED` | not something Chip can request |
| `setdone {id:1, done:true}` under `write` | success; Vikunja stored `done=1`, `done_at` set, other fields unchanged | success |
| `setdone {id:4}` under `write` | `FORBIDDEN` (Vikunja) | refused |
| `setdone {id:3}`, grant has write but token cannot update | `UNAUTHORIZED` (Vikunja's 401) | refused |

AppPort's grant refusal and Vikunja's refusal carry the same code (`FORBIDDEN`). Chip can still
attribute them, because it offers only tools whose declared scopes are within the grant: any
refusal it observes can only come from the application.

## 4. Stack-Agent runs with real models (measured)

`examples/mcp_work.rs` runs Chip's work loop exactly as `chip-experiments` composes a replay run:

* only the source's descriptors;
* `NoLocalPolicy`, `ModelDecisionBoundary`, `FullEscalationContext`;
* the required observation in the loop;
* no evidence reuse.

The model is real, built by `stack-agent`'s own provider builder with `work`'s defaults:

* the decision schema as `response_format` (`full`; `flat` for OpenAI, which refuses a root `anyOf`);
* temperature 0;
* 2048 output tokens.

Capability results come from the recording: a request that was not recorded fails, and nothing is
substituted for it. Every reply is saved to `fixtures/mcp/model-run.*.json`. `tests/mcp_replay.rs`
then replays each run through `chip-experiments`' `Runner`. The baseline has no source; the
candidate has it.

| Task (goal ends "…summary must end with `done=…; project=…`") | Model | What the loop did | Outcome |
| --- | --- | --- | --- |
| Is task 1 done, and in which project? | Qwen3-Coder-30B | `projects.list {}` → success; `tasks.get {id:1}` → success | **completed `done=false; project=Garden`**, Verified |
| same | gpt-4.1 | `tasks.get {id:"1"}` → `INVALID_INPUT`; `tasks.get {id:1}` → success; `projects.list {}` → success | **recovered from the contract's refusal**; completed, Verified |
| Is task 4 done? (carol's task) | Qwen3-Coder-30B | none: claimed `project=Test Project` | **completion refused** ("no authoritative observation satisfies the goal") |
| same, reworded | Qwen3-Coder-30B | `tasks.get {id:4}` → **refused, FORBIDDEN** | **escalated** ("Access forbidden…"), no answer claimed |
| Is task 999999 done? | Qwen3-Coder-30B | `tasks.get {id:999999}` → **NOT_FOUND** | **blocked** ("the task does not exist") |
| Mark task 1 done | Qwen3-Coder-30B ×3 | none; `projects.list` only; two JSON objects | refused, refused, invalid decision: never wrote |
| same | gpt-4.1 | `tasks.get {id:1}` → success (open); `tasks.setdone {id:1, done:true}` → success | **completed `done=true`**, Verified |

In the completed read and write runs, the baseline was given the same replies without the source.
It was offered nothing, executed nothing and completed nothing (asserted).

Runs that did not produce a fixture:

* two Qwen runs hit the gateway's limit of 5 requests per minute; Chip failed them without retrying
  and recorded `delivery: confirmed_response`;
* one Qwen write run requested `projects.list` in the write phase, which had not been recorded then.
  Replay refused it ("not in the recording"); the phase now records listing.

## 5. Independent evidence (measured)

`ApplicationAgrees` judges each MCP observation by Vikunja's own tables, read right after the
recorded call that answered it:

* a success's result fields must equal the stored row (title, done, project, creator), and a
  listing must contain exactly the caller's stored projects;
* a refusal must concern a row that exists and that the caller does not own;
* `NOT_FOUND` must concern a row that is not stored;
* the contract's own declared verification (`setdone`'s `integration.verify`: `tasks.get` must show
  the requested `done`) is evaluated on the stored row.

`AnswerStates` checks the completion's `done=` and `project=` against values the test reads from
those tables, not from the model.

Each check is shown to catch tampering:

* a stored title changed: the result is caught as wrong;
* task 4 re-owned to bob: the refusal is caught as unexplained;
* the stored `done` flipped: the write and its declared verification fail.

Removing the grant filter, or reading `isError` results as successes, fails 4 and 7 of the 15 tests
respectively.

Tests: `cargo test -p chip-appport-source`: **15/15** (`mcp_replay.rs`) and **15/15** (`replay.rs`,
R1, unchanged). `scripts/audit-dependencies.sh` passes.

## 6. Remaining blockers and limits

1. **No live mode.** The loop never calls the MCP server; the recorder does. Calling it from inside a
   run is new authority: every tool is unconfined and the write changes Vikunja. It needs its own
   `AGENTS.md` §14 case, including recovery of an interrupted `setdone`, which the contract declares
   `idempotency: unsupported` with `INTERNAL_ERROR` → "read before retrying".
2. **Replay is stateless per request.** `chip-experiments` answers each `(capability, inputs)` with
   the first recording that matches. A model that wrote and then read the task back would see the
   pre-write answer. gpt-4.1 did not read back. A live mode, or an ordered recording in
   `chip-experiments`, would remove this limit.
3. **Coverage.** A request the recorder did not anticipate fails the run (seen once). Only a live
   mode removes this.
4. **The bridge does not serve MCP itself.** Its process serves AppPort HTTP only. Serving it over
   MCP took a host that knows bridge-kit's session attribute for the forwarded credential
   (`appport.bridge.credential`, not exported). The fix belongs in the bridge kit, in the fork
   repositories, which were not changed: an option to mount `createMcpHandler`. In HTTP mode,
   bridge-kit grants all its scopes to any credential holder; here the operator's grant is narrower.
5. **Same code for two refusals.** AppPort's grant refusal and the application's refusal are both
   `FORBIDDEN`, distinguishable only by message text or by the grant reasoning above. A
   `details.source` (or a distinct code) in AppPort would make this explicit.
6. **No durable evidence from the bridge.** It returns only a trace id and stores nothing. Here,
   independent evidence relies on reading Vikunja's database, which the operator of a disposable
   instance can do. Elsewhere it would have to be the application's own read API (the contract's
   `verify`), which is weaker.
7. **Model variance, one run each.** Qwen3-Coder-30B never wrote in three attempts. gpt-4.1 wrote on
   the first. These runs show the integration and its fail-closed behavior. They are not evidence of
   effectiveness.
8. Pre-existing, unchanged: `stack-agent`'s `product_path` (2 failures: `telemetry::report`,
   `chip_otel`) and one clippy lint in R1's `EvidenceCorrelated`.

## Reproducing

```sh
S=$(mktemp -d); cd $S
git clone https://github.com/rkendel1/vikunja && cd vikunja && git checkout 851a9da
mkdir -p frontend/dist && echo '<!doctype html>' > frontend/dist/index.html
go build -tags osusergo -o $S/vikunja-bin .
(cd appport-bridge && npm ci && VIKUNJA_BIN=$S/vikunja-bin npm test)      # 13/13
cd <stack-agent>/crates/chip-appport-source
node recorder/record-mcp.mjs --bridge $S/vikunja/appport-bridge --vikunja-bin $S/vikunja-bin --out fixtures/mcp
cargo test -p chip-appport-source
# a real-model run (writes the replies the replay test reads)
CHIP_API_KEY=... cargo run --release -p chip-appport-source --example mcp_work -- \
  --snapshot fixtures/mcp/vikunja.write.mcp.json --schema flat --model openai/gpt-4.1 \
  --endpoint https://ai-gateway.vercel.sh/v1/chat/completions \
  --goal "Use the Vikunja application to mark Vikunja task 1 as done. When you complete, the summary must end with exactly: done=<true or false>" \
  --require appport.vikunja.vikunja_tasks_setdone --out fixtures/mcp/model-run.write.gpt-4.1.json
```

Re-recording keeps ids and outcomes, so the committed replies stay valid. Trace ids and timestamps
change.
