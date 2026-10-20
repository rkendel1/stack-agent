# AppPort generation G1: can the published packages turn an Express app into callable capabilities?

**Question.** Using only published npm packages (`appport@1.1.20`, `@appport/sdk@1.3.0`,
`@appport/mcp@1.3.2`) and no hand-written adapters, can AppPort discover a real operation in an
existing application, bind it to the existing implementation, expose it through MCP, and let
Stack-Agent invoke it?

**Answer: no, today.** Discovery works on a suitable Express app. Binding does not: every generated
binding fails before it reaches the application, and the generated boundary is not an application
`@appport/mcp` can serve. Per the agreed rule, no adapter was written to make it work. Labels:
**measured** (run), **read** (from the published code).

Upstream was not modified: the target was cloned into a disposable directory with push disabled, the
generator ran in a separate copy, and the clone's tracked tree is unchanged.

## 1. Compatibility gate (read)

`go2ismail/Invoice` (ASP.NET Core 2 / C#) is out of scope for the published analyzer:
`discoverCapabilityCandidates` (`appport/dist/commands.js`) reads only `.js/.ts(x)`, `.rs`, `.swift`,
`.kt`, `.java`, `.dart`, `.py` and `.go`, and its HTTP patterns are Express-style
`(app|router|server).(get|post|put|patch|delete)("…")`, Node `http` path checks and Go `HandleFunc`.
There is no reference to C#, `.csproj`, `dotnet`, ASP.NET, PHP or Laravel in the CLI.

## 2. Candidate screen (measured: the published analyzer run on each candidate's files)

Files were read from jsDelivr's mirror of each public repository (no clone, no install), and
`appport@1.1.20`'s own exported `discoverCapabilityCandidates` was run over them.

| Candidate | Stack | Analyzer result | Local data | License | Verdict |
| --- | --- | --- | --- | --- | --- |
| `ranazaeem1/invoice-approval-desk` | TS, Express 4, better-sqlite3; 4 server files, 2 runtime deps; 19 tests | `GET /api/invoices`, `GET /api/invoices/:id`, `PATCH /api/invoices/:id/status`; full paths | SQLite file, deterministic seed (6 invoices, fixed ids and statuses) | `ISC` in `package.json` only, no LICENSE file | **selected**: smallest, both target operations (read by id; status change validated by a transition table, 400/409) |
| `Future6524/invoice-generator` | TS, Express 5, better-sqlite3, React, Vite, pdfkit | GETs only: `POST /api/invoices` and `DELETE /api/invoices/:id` are lost to name collisions with their GETs | SQLite, demo seed | MIT | invoices have no status |
| `theminji/invoiceflow` | JS, Express 5, better-sqlite3, `ethers` | 15 routes, full paths | SQLite | MIT | no tests; an orders/pricing SaaS shell |
| `MuhammadHamzaFarooq/mini-invoice-approval-desk` | TS, Express, Drizzle | `GET /health` only | SQLite | MIT | routes not matched |
| `Daksh-codes/invoice-generator` | JS, Express | 28 routes, mount prefixes lost (`/:id`, `/new`) | SQLite | MIT | targets would not reach the routes |
| `asimhashmi47/quick-invoice` | JS, Express | 4 routes, prefixes lost | SQLite | MIT in `package.json` and README | prefixes lost |
| `shakeelahmed4007-design/Invoice-Desk` | TS, Express | 3 routes, prefixes lost | SQLite | none | no license |
| `micrometre/express-invoice` | JS, Express | 3 routes | SQLite | `MIT` in `package.json` | no customer/status reads |

Analyzer limits seen across candidates: routes on a mounted router lose the mount prefix, and
candidates are keyed by a name derived from the path, so a `POST` and a `GET` on the same path collide
and one is dropped.

## 3. Run on the selected candidate (measured)

`ranazaeem1/invoice-approval-desk` at `f7291e8`. Upstream's `npm ci` fails (the lockfile pins
Express 5 and `@types/*` versions that `package.json` does not allow); installing from `package.json`
without writing a lockfile gives Express 4.22.3, and upstream's tests pass (19/19). The server was
started with a scratch database. Ground truth: invoice 3 is `Harbor Legal LLP`, `needs_review`;
`approved → rejected` is 409; a missing invoice is 404.

| Step | Command (published CLI) | Result |
| --- | --- | --- |
| UNDERSTAND, DISCOVER | `appport app` | Node.js / TypeScript, Express; `npm start`; **3 invocation surfaces**: `invoices`, `invoices.id`, `invoices.id.status`. Then stops at an interactive prompt |
| Contract and bridge | `appport add --no-build --no-verify --name … --id com.example.invoice-desk --capability invoices,invoices.id,invoices.id.status` | Contract with 3 capabilities; `appport.bridge.json` with the correct bindings (`GET /api/invoices`, `GET /api/invoices/:id`, `PATCH /api/invoices/:id/status`); generated runtime and implementation; "Implementation is structurally consistent with the authoritative contract"; "Standalone AppPort setup complete" |
| Schemas | (generated contract) | every input and output schema is an empty open object: no `id` input is declared |
| Execution | the generated `appport/implementation/index.ts`, called as generated | **all three bindings throw `ERR_INVALID_URL`**: the generated bridge calls `fetch("/api/invoices/:id", { method: "POST", … })`, a bare path with no origin |
| Execution, if the origin were supplied | the same calls sent to the running server | the bridge always uses `POST` (the binding's `GET`/`PATCH` is not passed to it) and never substitutes `:id`: "list invoices" reaches `POST /api/invoices`, the **create** route (refused only because the body was not a valid invoice); the other two reach `POST /api/invoices/:id…` with a literal `:id` and get 404 |
| Runtime | `appport dev` | refused: "Application onboarding is incomplete: appport/server.ts does not exist", a file `add` never generates |
| MCP | `@appport/mcp` `createMcpServer` / `createMcpHandler` / `createMcpBridge` | need an `AppPortApplication`; the generated `createUniversalBoundary` returns `Object.freeze({ implementation })`, which is not one. Packaging (`app package`) needs a WASM implementation; the generated WASM is a scaffold ("behavior is not promoted") |

Nothing in the application changed: invoice 3 was still `needs_review` afterwards.

## Conclusion

| Required | Published packages today |
| --- | --- |
| Discover a real operation | **yes**, for literal Express routes on `app`/`router`/`server` with full paths |
| Bind it to the existing implementation | **no**: no origin, always `POST`, path parameters not substituted, empty schemas; one read is bound to a write route |
| Expose it through MCP | **no**: the generated boundary is not an `AppPortApplication`, and `appport dev` cannot load it |
| Stack-Agent invokes it | not reached |

The gap is in AppPort's generated HTTP bridge and boundary, not in Stack-Agent: the replay source
(`../appport-source-r1/`) consumes any host that serves discovery and capability calls. Closing it
belongs in AppPort (pass the binding's method, resolve an origin, substitute path parameters from the
request, emit a loadable application), and should be measured with this same run.

## 4. AppWorks (measured)

`rkendel1/appworks` at `9737baa` has the missing binding, in an **unpublished** package.

* `@appport/assessment` and `@appport/extraction` (not on npm) do not help here: the assessment analyzes
  only Go and Protocol Buffers (TypeScript and JavaScript are counted, not analyzed), and an extraction
  "is not executable"; its adapters are hand-written (`extractions/memos-reactions`).
* `@appport/workspace` (private, `"private": true`) does. Its route scan (`src/surface.ts`: Go, Express-style
  JS/TS, Flask/FastAPI) and installation generator (`src/install.ts`, templates `capabilities.mjs`,
  `server.mjs`) were run from source over the pristine clone, writing outside it:
  * **4 routes** found, each cited to its line, method kept in the name (`api.getinvoicesbyid`,
    `api.patchinvoicesstatusbyid`, `api.postinvoices`, `api.getinvoices`): no name collision;
  * generated `capabilities.mjs`: one `defineCapability` (published `@appport/sdk@1.2.2`) per route, calling
    `APPPORT_UPSTREAM` (inferred `http://127.0.0.1:3000`) with the route's own method, `{id}` filled from
    `params` and URL-encoded, the origin enforced, no redirects, a 30 s timeout; GET is `observation` with
    scope `api.read`, the others `consequential` with `api.write`;
  * generated `server.mjs`: an AppPort HTTP server, not MCP. Because the application is a real
    `AppPortApplication`, the published `@appport/mcp@1.3.2` `createMcpHandler` serves it through a generic
    host that loads `capabilities.mjs` unchanged; the caller's permissions are the operator's grant.

Through MCP, against the running app:

| Call | Result |
| --- | --- |
| `tools/list` | 4 tools with scope and effect; under a read-only grant the write tools are still listed |
| `api_getinvoicesbyid {"params":{"id":"3"}}` | the real row: Harbor Legal LLP, `needs_review` |
| status change, grant `api.read` only | refused before the app (`UNAUTHORIZED`, "Authentication is required", not a forbidden-scope error) |
| `api_patchinvoicesstatusbyid` 1 → approved, grant `api.read api.write` | done; the database row changed (`approved`, note "via MCP") |
| 5 approved → rejected | **`isError: false`** with `{"status":409,…}`: the app's refusal is returned as a successful call |
| missing invoice 99 | **`isError: false`** with `{"status":404,…}` |
| missing path parameter | `isError: true`, `INTERNAL_ERROR` with no reason |
| `{"id":"../../etc"}` | encoded, stayed on the origin; the app answered 400 |
| wrong token | refused |

So AppWorks discovers, binds and (with published `@appport/mcp`) exposes the real operations. Its gaps:
it is not published; schemas are generic (`params`/`query`/`body` records), so the contract does not say
that `id` is required; an HTTP error status is a successful capability result, which a consumer must not
read as success; and nothing records an execution unit beyond the SDK's trace id.

## Reproducing

```sh
S=$(mktemp -d)
git clone --depth 1 https://github.com/ranazaeem1/invoice-approval-desk $S/repo   # f7291e8
cp -r $S/repo $S/work && cd $S/work
npm install --no-package-lock && npm test
(DB_PATH=$S/app.db PORT=3000 npx tsx src/server.ts &)
npm i --prefix $S/cli appport@1.1.20 @appport/mcp@1.3.2
$S/cli/node_modules/.bin/appport app < /dev/null
$S/cli/node_modules/.bin/appport add --no-build --no-verify --name "Mini Invoice Approval Desk" \
  --id com.example.invoice-desk --version 1.0.0 --capability invoices,invoices.id,invoices.id.status < /dev/null
$S/cli/node_modules/.bin/appport dev --port 3901 --no-studio < /dev/null
```
