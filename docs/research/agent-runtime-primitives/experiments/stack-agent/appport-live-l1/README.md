# AppPort live L1: a generated bridge, a live MCP endpoint, Chip, and the application's own state

This record follows `../appport-generation-g2/` (a generated bridge could not reach MCP) and
`../appport-mcp-m1/` (Chip consumed a hand-written bridge, in replay). It tests the claim neither
established:

> An existing application's routes are generated into a governed AppPort bridge, an MCP client and
> then Chip's work loop discover and call it live, the application's state changes as authorized,
> refusals change nothing, an interrupted or ambiguous write is never blindly repeated, and every
> verdict is checked against the application's own database.

**Answer: yes, for the invoice desk (Express, SQLite), with a scripted model, under the conditions
below.** Nothing generated was edited by hand. Four defects found on the way were fixed where they
live, each with a regression test; they are listed in §4.

Labels: **measured** (run here, evidence under `artifacts/`), **deterministic** (no model is
involved), **model-dependent** (none here: §6).

## 1. What ran

| | |
| --- | --- |
| Application | `ranazaeem1/invoice-approval-desk` at `f7291e8`, fresh clone, `npm install --no-package-lock` (its lock file does not satisfy its `package.json`, so `npm ci` refuses it); tracked files unchanged |
| Generator, `appport dev`, MCP endpoint | `rkendel1/appboundry` at `ceff445` (clean), packed with the repository's own release staging (`artifacts/harness/pack-local.mjs`) and installed into the application as `appport@1.1.20` and `@appport/sdk@1.1.20` tarballs; nothing published |
| Generation | `appport add --name "Mini Invoice Approval Desk" --id com.example.invoice-desk --version 1.0.0 --capability invoices,invoices.id,invoices.id.status --no-build --no-start --no-verify`; generated file digests in `artifacts/stage2/generated.sha256` |
| Endpoint | `appport dev`, `POST /apps/com.example.invoice-desk/mcp`; two instances: grant `invoices.read`, and grant `invoices.read invoices.write`, each with its own `APPPORT_MCP_TOKEN`; upstream through a counting proxy that logs every request reaching the application |
| Generic MCP client (stage 2) | `@modelcontextprotocol/sdk` 1.31.0 `Client` + `StreamableHTTPClientTransport` |
| Chip (stage 3) | `rkendel1/stack-agent` at `30c5678`: `examples/mcp_live.rs` (Chip's work loop, `chip_appport_source::live::LiveMcp`, `chip_agent_state::DurableWork` on a FeltDB file per task, a scripted model); binary digests in `artifacts/stage3/driver-binaries.sha256` |
| Independent evidence | the application's SQLite database read directly (`sqlite3`, read-only) before and after every case, and the proxy's log of requests that reached the application |
| Node / npm | v22.22.0 / 10.9.4 |

Reproduce: `artifacts/harness/run-e2e.sh <appboundry checkout> <out>` (stage 2), then
`artifacts/harness/stage3.py <out>/work/desk <mcp_live> <stack-agent> <out3>` (stage 3).

## 2. Stage 2: generated bridge → MCP → generic client (measured, deterministic): 12/12

Generated contract (`artifacts/stage2/appboundry.contract.json`):

| Capability | Route | Requires | Inputs |
| --- | --- | --- | --- |
| `invoicedesk.invoices@1` | `GET /api/invoices` | `invoices.read` | `status` |
| `invoices.id@1` | `GET /api/invoices/:id` | `invoices.read` | `id` (required) |
| `invoices.id.status@1` | `PATCH /api/invoices/:id/status` | `invoices.write` | `id` (required), `status`, `note` |

| Check | Result |
| --- | --- |
| No token | HTTP 401 |
| Read-only grant: `tools/list` | the two reads only |
| Read-write grant: `tools/list` | the three tools, with `appboundry/authorization` |
| Read invoice 3; list `status=needs_review` | equal to the database rows |
| Read-only grant calls the write | `FORBIDDEN`, `missingScopes: ["invoices.write"]`, no execution evidence, database unchanged |
| Read-write grant: invoice 1 → `approved` | succeeded; the database shows it; evidence `executionMode: development`, an execution unit |
| Illegal transition, missing invoice, missing path parameter | `CONFLICT`, `NOT_FOUND`, refused; database unchanged |

`artifacts/stage2/mcp-cases.json`, `db-before.json`, `db-after.json`, `run.log`.

## 3. Stage 3: Chip against the live endpoint (measured, deterministic): 47/47

`artifacts/stage3/results.json`; each case's run, scripted replies and FeltDB record beside it.

| Case | What was shown |
| --- | --- |
| 1 Discover and read (4) | offered = the grant's two read tools, from `tools/list`; invoice 3's observation equals its row; exactly one GET reached the application |
| 2 Authorized write (5) | invoice 2 → `approved`, note "approved by chip"; exactly one PATCH; the observation's receipt id is the host's execution unit; FeltDB records the task completed |
| 3 Forbidden (5) | read-only endpoint: the write is not listed, so not offered; read-write endpoint under a read-only operator grant: listed, not offered; neither run completes; no write reached the application |
| 4 Not found, then a fabricated answer (2) | `NOT_FOUND` is a failed observation with the code; "Invoice 999 is approved" is refused |
| 5 Interrupted write, restart (8) | abort after the endpoint answered: the write took effect once, the restart is refused `OutcomeUncertain` (exit 3) and sends nothing; abort before sending: nothing reached the application, the restart is still refused (Chip cannot know), nothing sent |
| 6 No call, malformed reply, malformed or lost result (17) | completion without the required call refused; a reply that is not a decision is not a completion; a non-JSON body, a JSON-RPC message with no result, and a connection dropped after the write executed each end the work `Failed` with "its outcome is unknown", record no outcome (the intent stays open), and are never repeated: the hang-up's write reached the application once, and the resumed attempt is refused `OutcomeUncertain` and sends nothing |
| 7 Evidence distinguishes (6) | application failure: `failed` + `CONFLICT`; unreachable at connect: the run does not start; unreachable at call: `ExecutorUnavailable`, "nothing was sent", the intent resolved; sent with no answer: `OutcomeUnknown`, the intent open; refusal: not offered, with the reason (`c7-evidence-kinds.json`) |

Over the whole run four requests other than GET reached the application (`upstream-requests.json`):
invoice 2 (case 2), invoice 4 (case 5, once), invoice 3 (case 6, once) and invoice 5 (case 7, which
the application refused with 409). Invoice 1, the target of every forbidden write and of the
crash-before-send case, never changed (`db-before.json`, `db-after.json`).

## 4. Defects found and fixed (each with a test that fails without the fix)

| Where | Defect | Fix |
| --- | --- | --- |
| appboundry `7a22ca0` | Release staging rewrote import lines inside the CLI's code templates, so a packaged CLI generated `appport/server.ts` (and Electron/Tauri entries) importing `../vendor/@appport/sdk/...`, which does not exist in the user's project | rewrite only real import/export declarations (`release-staged-generation.test.ts`) |
| appboundry `0e735cb` | every generated capability of a prefix required one `<prefix>.invoke`: a read could not be granted without the write; `appport dev` had no MCP endpoint | `<resource>.read` / `.write` from the route's method; `appport dev` serves the host's existing MCP endpoint (`http-bridge.test.ts`) |
| appboundry `ceff445` | generated inputs named only path parameters, so a client sending only declared inputs (Chip) could not send the write's body (`capability does not accept input 'note'`) | declare the body and query fields the Express handler reads (`http-bridge.test.ts`) |
| stack-agent `b120838` | an executor error was always recorded as the action's outcome: a write whose answer was lost was recorded as failed, and **the resumed attempt sent it again** | `ExecutionError::OutcomeUnknown`, for which durable state records no outcome (`tests/outcome_unknown.rs`) |

## 5. What this does not establish

* One application, one framework (Express-style), SQLite, local processes on Linux x86_64. Go,
  Django and Flask routes are still not discovered (`../appport-generation-g2/`).
* No effect is declared in the generated contract: the AppBoundry contract schema has no field for
  it. Read and write are separated only by the permission the method implies; Chip treats every MCP
  tool as consequential regardless.
* Scopes mode only. The grant is the endpoint's (one per token), not per caller; AuthBoundry
  authority mode is exercised by AppBoundry's own tests, not here. The application authenticates
  nobody, so the bridge forwards no caller credential, and that gap remains for applications that do.
* `appport dev` is a development endpoint (`executionMode: development`, unpackaged). Packaging a
  generated TypeScript bridge, and hosting it beyond `appport dev`, remain unsupported.
* Recovery is fail-closed, not automatic: an uncertain write stops the task until an operator
  settles it (`stack-agent agent-state settle`); settlement was not exercised here.
* `examples/mcp_live.rs` composes Chip's loop and durable state as `stack-agent work` does, but it is
  not `stack-agent work`: no production command offers MCP capabilities.
* Interruption is an `abort()` at two named points in the driver, and transport faults come from a
  stub in front of the endpoint; no real network partition or kernel kill timing was explored.

## 6. Model-dependent results

None. Every model reply is scripted (`*.replies.json`), so these results are about Chip's runtime,
the transport, the endpoint and the application. Whether a real model chooses these calls, reads
the refusals correctly or stops at an uncertain outcome is not tested here.
