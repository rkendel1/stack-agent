// Records what an existing AppPort campaign bridge serves over MCP, for chip-appport-source's MCP replay.
//
//   node record-mcp.mjs --bridge <vikunja>/appport-bridge --vikunja-bin <vikunja binary> --out <fixtures dir>
//
// Nothing here names a capability to Chip and nothing is written into the bridge or Vikunja's repository:
//
// * a disposable Vikunja (the real server, SQLite in a temp dir) is seeded through its own v1 API by the operator;
// * the bridge is served over MCP from its own code and its own pinned packages: the capabilities its `bridge.mjs`
//   exports, the application and resources its unchanged process serves in its manifest, `createApplication` and
//   `permissionAuthorizer` from its `@appport/sdk` (1.2.2) and `createMcpHandler` from the `@appport/mcp` (1.3.0)
//   that SDK depends on. The MCP session's permissions are the operator's grant; the Vikunja token in the session is
//   the operator's configuration and is never written out (the bridge forwards it, as in trusted-boundary mode);
// * every MCP request and response is recorded as sent and received (JSON-RPC over Streamable HTTP);
// * Vikunja's own tables are read directly, read-only, after each phase: the oracle a consumer's claims are checked
//   against: after every call (each answer is judged by the state at its moment) and at the end of each phase.

import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

const args = Object.fromEntries(process.argv.slice(2).reduce((a, v, i, all) => (v.startsWith("--") ? [...a, [v.slice(2), all[i + 1]]] : a), []));
for (const k of ["bridge", "vikunja-bin", "out"]) if (!args[k]) throw new Error(`--${k} is required`);
const BRIDGE = resolve(args.bridge);
const OUT = resolve(args.out);
const SCHEMA = "chip.appport-mcp-snapshot.v1";
const CREDENTIAL = "appport.bridge.credential"; // the session attribute bridge-kit's callApplication reads

const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8" }).trim();
const pkg = (name) => JSON.parse(execFileSync("node", ["-p", `JSON.stringify(require("${BRIDGE}/node_modules/${name}/package.json").version)`], { encoding: "utf8" }));

const work = mkdtempSync(join(tmpdir(), "chip-appport-mcp-"));
const children = [];
process.on("exit", () => { for (const c of children) try { c.kill("SIGKILL"); } catch {} rmSync(work, { recursive: true, force: true }); });

async function freePort() {
  return new Promise((r) => { const s = createServer(); s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => r(port)); }); });
}
async function waitFor(url) {
  for (let i = 0; i < 300; i++) { try { await fetch(url, { signal: AbortSignal.timeout(1000) }); return; } catch {} await new Promise((r) => setTimeout(r, 200)); }
  throw new Error(`${url} never answered`);
}
async function http(base, method, path, { body, headers = {} } = {}) {
  const res = await fetch(new URL(path, base), { method, headers: { accept: "application/json", ...(body !== undefined ? { "content-type": "application/json" } : {}), ...headers }, ...(body !== undefined ? { body: JSON.stringify(body) } : {}) });
  const text = await res.text();
  let json = null; try { json = JSON.parse(text); } catch {}
  return { status: res.status, json };
}

// ---- Vikunja, seeded by the operator ------------------------------------------------------------------------------

const vport = await freePort();
const VIKUNJA = `http://127.0.0.1:${vport}`;
const root = join(work, "vikunja"); mkdirSync(root);
const db = join(root, "vikunja.db");
const vikunja = spawn(resolve(args["vikunja-bin"]), ["web"], { stdio: "ignore", env: {
  ...process.env, HOME: root, VIKUNJA_SERVICE_INTERFACE: `127.0.0.1:${vport}`, VIKUNJA_SERVICE_ROOTPATH: root, VIKUNJA_SERVICE_PUBLICURL: `${VIKUNJA}/`,
  VIKUNJA_SERVICE_SECRET: randomBytes(24).toString("hex"), VIKUNJA_DATABASE_TYPE: "sqlite", VIKUNJA_DATABASE_PATH: db,
  VIKUNJA_FILES_BASEPATH: join(root, "files"), VIKUNJA_SENTRY_ENABLED: "false", VIKUNJA_MAILER_ENABLED: "false"
} });
children.push(vikunja);
await waitFor(`${VIKUNJA}/api/v1/info`);
const vikunjaVersion = (await http(VIKUNJA, "GET", "/api/v1/info")).json?.version ?? null;

const READ = { projects: ["read_all", "read_one"], tasks: ["read_one"] };
const FULL = { projects: ["read_all", "read_one"], tasks: ["read_one", "update"] };
const users = {};
for (const [username, password] of [["bob", "synthetic-bob-pw-1"], ["carol", "synthetic-carol-pw-2"]]) {
  if ((await http(VIKUNJA, "POST", "/api/v1/register", { body: { username, email: `${username}@example.invalid`, password } })).status !== 200) throw new Error(`register ${username}`);
  const jwt = (await http(VIKUNJA, "POST", "/api/v1/login", { body: { username, password } })).json.token;
  const token = async (permissions) => `Bearer ${(await http(VIKUNJA, "PUT", "/api/v1/tokens", { headers: { authorization: `Bearer ${jwt}` }, body: { title: "chip mcp recording", expires_at: new Date(Date.now() + 86_400_000).toISOString(), permissions } })).json.token}`;
  users[username] = { jwt, read: await token(READ), full: await token(FULL) };
}
const as = (who) => ({ authorization: `Bearer ${users[who].jwt}` });
const seed = {};
for (const [who, project, tasks] of [
  ["bob", "Garden", [["Water the turnips", false], ["Prune the roses", true]]],
  ["bob", "Kitchen", [["Descale the kettle", false]]],
  ["carol", "Studio", [["Varnish the shelf", false]]]
]) {
  const p = (await http(VIKUNJA, "PUT", "/api/v1/projects", { headers: as(who), body: { title: project } })).json;
  for (const [title, done] of tasks) {
    const t = (await http(VIKUNJA, "PUT", `/api/v1/projects/${p.id}/tasks`, { headers: as(who), body: { title, description: "synthetic task", priority: 2 } })).json;
    if (done && (await http(VIKUNJA, "PATCH", `/api/v2/tasks/${t.id}`, { headers: as(who), body: { done: true } })).status !== 200) throw new Error(`seed done ${title}`);
    seed[title] = t.id;
  }
}

const { DatabaseSync } = await import("node:sqlite");
function oracle() {
  const o = new DatabaseSync(db, { readOnly: true });
  try {
    return {
      tasks: o.prepare("SELECT t.id, t.title, t.description, t.project_id, t.done, t.done_at, t.priority, u.username AS created_by FROM tasks t JOIN users u ON u.id = t.created_by_id ORDER BY t.id").all()
        .map((r) => ({ ...r, done: Boolean(r.done), done_at: r.done_at && !String(r.done_at).startsWith("0001-01-01") ? r.done_at : null })),
      projects: o.prepare("SELECT p.id, p.title, p.is_archived, u.username AS owner FROM projects p JOIN users u ON u.id = p.owner_id ORDER BY p.id").all()
        .map((r) => ({ ...r, is_archived: Boolean(r.is_archived) }))
    };
  } finally { o.close(); }
}

// ---- the bridge, as its own process serves it (for its application and resources) ---------------------------------

const bport = await freePort();
const bridgeProcess = spawn("node", [join(BRIDGE, "bridge.mjs")], { stdio: "ignore", env: { ...process.env, APPPORT_BRIDGE_UPSTREAM: VIKUNJA, APPPORT_BRIDGE_PORT: String(bport), APPPORT_BRIDGE_PUBLIC: "1" } });
children.push(bridgeProcess);
await waitFor(`http://127.0.0.1:${bport}/.well-known/appport`);
const manifest = (await http(`http://127.0.0.1:${bport}`, "GET", "/appport/manifest", { headers: { authorization: users.bob.read } })).json;
bridgeProcess.kill("SIGTERM");
const integration = manifest.attributes["appport.integration/1"];

const { createApplication, permissionAuthorizer } = await import(`${BRIDGE}/node_modules/@appport/sdk/dist/index.js`);
const { createMcpHandler } = await import(`${BRIDGE}/node_modules/@appport/mcp/dist/index.js`);
const { capabilities } = await import(`${BRIDGE}/bridge.mjs`);
const upstream = { baseUrl: `${VIKUNJA}/`, headers: (credential) => ({ authorization: credential }) };
const application = createApplication({ application: manifest.application, capabilities: capabilities(upstream), resources: integration.resources, authorizer: permissionAuthorizer() });

// ---- one MCP host per operator configuration: grant (AppPort scopes) + the Vikunja token it forwards ---------------

async function mcpHost({ grant, credential }) {
  const bearer = randomBytes(24).toString("hex");
  const handler = createMcpHandler(application, {
    async context({ headers }) {
      if (headers.authorization !== `Bearer ${bearer}`) return undefined;
      const principal = { id: "operator", type: "user" };
      return { principal, session: { id: randomUUID(), principal, applicationId: manifest.application.id, createdAt: new Date().toISOString(), permissions: grant, attributes: { [CREDENTIAL]: credential } } };
    }
  });
  const server = createServer(async (req, res) => {
    const chunks = []; for await (const c of req) chunks.push(c);
    const response = await handler(new Request(`http://127.0.0.1${req.url}`, { method: req.method, headers: req.headers, body: req.method === "POST" ? Buffer.concat(chunks) : undefined }));
    res.writeHead(response.status, Object.fromEntries(response.headers));
    res.end(Buffer.from(await response.arrayBuffer()));
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const url = `http://127.0.0.1:${server.address().port}/mcp`;
  let id = 0;
  const rpc = async (method, params, token = bearer) => {
    const message = { jsonrpc: "2.0", id: ++id, method, ...(params !== undefined ? { params } : {}) };
    const res = await fetch(url, { method: "POST", headers: { "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-06-18", authorization: `Bearer ${token}` }, body: JSON.stringify(message) });
    const body = await res.json();
    return { request: message, http_status: res.status, response: body };
  };
  return { rpc, close: () => new Promise((r) => server.close(r)) };
}

const describeCredential = { [users.bob.read]: "bob: Vikunja API token, projects read_all/read_one, tasks read_one", [users.bob.full]: "bob: Vikunja API token, projects read_all/read_one, tasks read_one/update" };

async function record(label, { grant, credential }, calls) {
  const host = await mcpHost({ grant, credential });
  const initialize = await host.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "chip-appport-mcp-recorder", version: "1" } });
  const tools = await host.rpc("tools/list", {});
  const recorded = [];
  for (const c of calls) {
    const r = await host.rpc("tools/call", { name: c.tool, arguments: c.arguments }, c.wrongBearer ? "not-the-operator-token-000000" : undefined);
    // Vikunja's stored state right after this call: what this call's answer is judged against.
    recorded.push({ label: c.label, tool: c.tool, arguments: c.arguments, ...(c.wrongBearer ? { mcp_bearer: "wrong" } : {}), http_status: r.http_status, response: r.response, oracle: oracle() });
  }
  await host.close();
  const snapshot = {
    schema: SCHEMA, label,
    bridge: {
      repository: "https://github.com/rkendel1/vikunja", path: "appport-bridge", commit: git(BRIDGE, "rev-parse", "HEAD"),
      packages: { "@appport/sdk": pkg("@appport/sdk"), "@appport/mcp": pkg("@appport/mcp"), "@modelcontextprotocol/sdk": pkg("@modelcontextprotocol/sdk") }
    },
    application: { name: "Vikunja", version: vikunjaVersion, built_from: git(BRIDGE, "rev-parse", "HEAD"), database: "sqlite (disposable)" },
    grant: { scopes: grant, credential: describeCredential[credential] },
    initialize: initialize.response.result,
    tools: tools.response.result.tools,
    calls: recorded,
    oracle: { source: "Vikunja's own tasks and projects tables, read directly (read-only) after the calls", ...oracle() }
  };
  const file = join(OUT, `vikunja.${label}.mcp.json`);
  writeFileSync(file, JSON.stringify(snapshot, null, 2) + "\n");
  for (const c of recorded) {
    const res = c.response.result ?? c.response.error;
    console.log(`${label} ${c.label}: ${c.response.error ? `jsonrpc error ${c.response.error.code}` : res.isError ? `isError ${res._meta?.["dev.appport/error"]?.code}` : "ok"}`);
  }
  return snapshot;
}

const A = seed["Water the turnips"], B = seed["Prune the roses"], C = seed["Descale the kettle"], D = seed["Varnish the shelf"];
const get = "vikunja_tasks_get", list = "vikunja_projects_list", setdone = "vikunja_tasks_setdone";

// Phase 1, read only: the operator grants the AppPort read scope and configures bob's read-only Vikunja token.
await record("read", { grant: ["vikunja.read"], credential: users.bob.read }, [
  { label: "get-own-open", tool: get, arguments: { id: A } },
  { label: "get-own-done", tool: get, arguments: { id: B } },
  { label: "get-own-other-project", tool: get, arguments: { id: C } },
  { label: "list", tool: list, arguments: {} },
  { label: "list-unarchived", tool: list, arguments: { includeArchived: false } },
  { label: "list-archived", tool: list, arguments: { includeArchived: true } },
  { label: "get-other-users-task", tool: get, arguments: { id: D } },
  { label: "get-missing", tool: get, arguments: { id: 999999 } },
  { label: "get-id-as-text", tool: get, arguments: { id: String(A) } },
  { label: "get-no-id", tool: get, arguments: {} },
  { label: "setdone-outside-grant", tool: setdone, arguments: { id: A, done: true } },
  { label: "get-wrong-mcp-bearer", tool: get, arguments: { id: A }, wrongBearer: true }
]);

// Phase 2, scoped write: the AppPort write scope, and bob's Vikunja token that may update tasks.
await record("write", { grant: ["vikunja.read", "vikunja.tasks.write"], credential: users.bob.full }, [
  { label: "list-before", tool: list, arguments: {} },
  { label: "list-unarchived-before", tool: list, arguments: { includeArchived: false } },
  { label: "get-before", tool: get, arguments: { id: A } },
  { label: "setdone-own", tool: setdone, arguments: { id: A, done: true } },
  { label: "setdone-other-users-task", tool: setdone, arguments: { id: D, done: true } },
  { label: "setdone-missing", tool: setdone, arguments: { id: 999999, done: true } }
]);

// Phase 3, the grant and the application disagree: AppPort grants write, Vikunja's token cannot update.
await record("write-token-read-only", { grant: ["vikunja.read", "vikunja.tasks.write"], credential: users.bob.read }, [
  { label: "setdone-token-cannot-update", tool: setdone, arguments: { id: C, done: true } }
]);

console.log(JSON.stringify({ seed, vikunja: vikunjaVersion, out: OUT }));
process.exit(0);
