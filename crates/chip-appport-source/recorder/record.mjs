// Records what a real AppPort host served, for the AppPort capability source's replay fixtures.
//
// Uses only published npm packages, pinned: the `appport` CLI (which builds, packages and hosts the
// application) and `@appport/sdk` (which the application source imports). The application is an
// AppPort application source plus its pre-built AppPort-ABI WASM artifact, pinned by digest. The
// recorder talks to the running host only over HTTP, as any client would: discovery
// (`/.well-known/appport`), capability invocation (`POST /apps/<selector>/capabilities/<name>@<v>`
// with the caller's `x-appport-scopes`), and the host's own usage API. It writes what the host
// returned, unmodified, plus the host's durable invocation and usage records.
//
//   node record.mjs <application-dir> <out-dir>
//
// Two hosts are recorded: one whose grant is `todos.read todos.write` and one granted `todos.read`
// only. Operator-run; never run by tests.

import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, mkdtempSync, readFileSync, readdirSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const APPPORT = "appport@1.1.20";
const SDK = "@appport/sdk@1.3.0";
const WASM_SHA256 = "e2e29af8cec7d6398aa453b6cbc1a8161831f6008ea060488723ac56cefca3ea";
const SELECTOR = "todos";

const [source, outDir] = process.argv.slice(2).map(p => p && resolve(p));
if (!source || !outDir) {
  console.error("usage: node record.mjs <application-dir> <out-dir>");
  process.exit(2);
}
const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
const wasm = readFileSync(join(source, "appport/wasm/application.wasm"));
if (sha256(wasm) !== WASM_SHA256) throw new Error(`application.wasm is ${sha256(wasm)}; the recorder pins ${WASM_SHA256}`);

// A scratch copy with the published packages installed and nothing else.
const work = mkdtempSync(join(tmpdir(), "appport-recorder-"));
const app = join(work, "app");
mkdirSync(join(app, "appport/wasm"), { recursive: true });
cpSync(join(source, "src"), join(app, "src"), { recursive: true });
writeFileSync(join(app, "appport/wasm/application.wasm"), wasm);
writeFileSync(join(app, "package.json"), JSON.stringify({ name: "todos", private: true, type: "module", dependencies: { "@appport/sdk": SDK.split("@").pop() }, devDependencies: { appport: APPPORT.split("@").pop() } }));
const run = (cmd, args, env = {}) => execFileSync(cmd, args, { cwd: app, env: { ...process.env, ...env }, encoding: "utf8" });
run("npm", ["install", "--no-audit", "--no-fund", "--silent"]);
const cli = join(app, "node_modules/.bin/appport");
run(cli, ["app", "build"]);
const packaged = run(cli, ["app", "package"]);
const packageIdentity = packaged.match(/(sha256:[0-9a-f]{64})/)?.[1];
if (!packageIdentity) throw new Error(`package printed no identity:\n${packaged}`);
const manifest = JSON.parse(readFileSync(join(app, "todos.app/manifest"), "utf8"));
const installed = name => JSON.parse(readFileSync(join(app, "node_modules", name, "package.json"), "utf8")).version;

async function call(origin, capability, input, scopes) {
  const response = await fetch(`${origin}/apps/${SELECTOR}/capabilities/${capability}`, {
    method: "POST",
    redirect: "error",
    headers: { "content-type": "application/json", "x-appport-scopes": scopes.join(" ") },
    body: JSON.stringify(input)
  });
  const text = await response.text();
  let body;
  try { body = JSON.parse(text); } catch { body = text; }
  return { http_status: response.status, execution_unit: response.headers.get("x-appboundry-execution-unit"), body };
}

function hostState(runtime) {
  const directory = join(runtime, "state");
  const records = [], usage = [];
  for (const file of existsSync(directory) ? readdirSync(directory).filter(f => f.endsWith(".json")).sort() : []) {
    const state = JSON.parse(readFileSync(join(directory, file), "utf8"));
    records.push(...(state.records ?? []));
    usage.push(...(state.usageRecords ?? []));
  }
  return { records, usage };
}

// `seed` calls are the operator's setup before any run (with the operator's own scopes); `calls`
// are made with the grant, the scopes the source will send.
async function record(label, grant, port, seed, calls) {
  const runtime = mkdtempSync(join(tmpdir(), `appport-recorder-${label}-`));
  const env = { APPPORT_RUNTIME_DIR: runtime };
  run(cli, ["host", "add", "todos.app"], env);
  const host = spawn(cli, ["host", "start", manifest.application.id, "--port", String(port)], { cwd: app, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  let output = "";
  host.stdout.on("data", d => { output += d; });
  host.stderr.on("data", d => { output += d; });
  const origin = `http://127.0.0.1:${port}`;
  try {
    let discovery = null;
    for (let attempt = 0; attempt < 120 && !discovery; attempt++) {
      try {
        const d = await (await fetch(`${origin}/.well-known/appport`, { redirect: "error" })).json();
        if (d.applications?.some(a => a.selector === SELECTOR)) discovery = d;
      } catch { /* not listening yet */ }
      if (!discovery) await new Promise(r => setTimeout(r, 500));
    }
    if (!discovery) throw new Error(`host never served discovery:\n${output}`);
    const recorded = [];
    for (const [name, capability, input, scopes] of seed) recorded.push({ label: name, operator: true, capability, input, scopes, ...(await call(origin, capability, input, scopes)) });
    for (const [name, capability, input] of calls) recorded.push({ label: name, operator: false, capability, input, scopes: grant, ...(await call(origin, capability, input, grant)) });
    const usageApi = await (await fetch(`${origin}/appboundry/api/usage`, { redirect: "error" })).json();
    return {
      schema: "chip.appport-snapshot.v1",
      label,
      recorded_by: "crates/chip-appport-source/recorder/record.mjs",
      appport_cli: `appport@${installed("appport")}`,
      appport_sdk: `@appport/sdk@${installed("@appport/sdk")}`,
      origin,
      package: { identity: packageIdentity, wasm_sha256: WASM_SHA256, application: manifest.application, contract_fingerprint: manifest.contractFingerprint, contract: manifest.contract },
      grant: { scopes: grant },
      discovery,
      calls: recorded,
      usage_api: usageApi,
      host_state: hostState(runtime)
    };
  } finally {
    host.kill("SIGTERM");
    await new Promise(r => host.once("exit", r));
  }
}

const OPERATOR = ["todos.read", "todos.write"];
const SEED = [
  ["seed-link", "todo.create@1", { title: "Fix the broken link in README.md" }, OPERATOR],
  ["seed-usage", "todo.create@1", { title: "Add a usage example" }, OPERATOR]
];
mkdirSync(outDir, { recursive: true });
const authorized = await record("authorized", ["todos.read", "todos.write"], 4473, SEED, [
  ["list", "todo.list@1", {}],
  ["complete", "todo.complete@1", { id: "todo_1" }],
  ["complete-unknown", "todo.complete@1", { id: "todo_99" }],
  ["undeclared-input", "todo.complete@1", { todo: "todo_1" }]
]);
writeFileSync(join(outDir, "todos.authorized.snapshot.json"), JSON.stringify(authorized, null, 1) + "\n");
const readOnly = await record("read-only", ["todos.read"], 4474, SEED, [
  ["list", "todo.list@1", {}],
  ["complete", "todo.complete@1", { id: "todo_1" }]
]);
writeFileSync(join(outDir, "todos.read-only.snapshot.json"), JSON.stringify(readOnly, null, 1) + "\n");
console.log(`recorded with ${authorized.appport_cli}, ${authorized.appport_sdk}, package ${packageIdentity}`);
