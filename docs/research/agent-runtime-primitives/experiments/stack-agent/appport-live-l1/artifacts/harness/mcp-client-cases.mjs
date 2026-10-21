// A generic MCP client (the official @modelcontextprotocol/sdk Client over Streamable HTTP) driving the generated
// invoice-desk bridge that `appport dev` serves. Every claim it prints about application state comes from reading the
// application's own SQLite database, never from the tool result.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import Database from "better-sqlite3";

const [readerUrl, readerToken, writerUrl, writerToken, dbPath] = process.argv.slice(2);
const db = new Database(dbPath, { readonly: true, fileMustExist: true });
const row = id => db.prepare("SELECT id, status, note, updated_at FROM invoices WHERE id = ?").get(id) ?? null;
const all = () => db.prepare("SELECT id, status, note, updated_at FROM invoices ORDER BY id").all();
const results = [];
const check = (name, ok, detail) => { results.push({ name, ok, ...detail }); console.log(`${ok ? "PASS" : "FAIL"} ${name}`); };

async function connect(url, token) {
  const client = new Client({ name: "e2e-generic-client", version: "1.0.0" });
  await client.connect(new StreamableHTTPClientTransport(new URL(url), { requestInit: { headers: { authorization: `Bearer ${token}` } } }));
  return client;
}
const call = (client, name, args) => client.callTool({ name, arguments: args });

const reader = await connect(readerUrl, readerToken);
const writer = await connect(writerUrl, writerToken);

// 1. Discovery: the read-only grant lists only the reads; the read-write grant lists all three.
const readerTools = (await reader.listTools()).tools;
const writerTools = (await writer.listTools()).tools;
const names = tools => tools.map(tool => tool.name).sort();
check("discovery: read-only grant lists only read tools", JSON.stringify(names(readerTools)) === JSON.stringify(["invoicedesk_invoices", "invoices_id"]), { tools: names(readerTools) });
check("discovery: read-write grant lists the generated read and write tools", JSON.stringify(names(writerTools)) === JSON.stringify(["invoicedesk_invoices", "invoices_id", "invoices_id_status"]), {
  tools: writerTools.map(tool => ({ name: tool.name, inputSchema: tool.inputSchema, meta: tool._meta }))
});

// 2. Read: the tool result agrees with the database row.
const read = await call(reader, "invoices_id", { id: "3" });
const readBody = read.structuredContent ?? JSON.parse(read.content[0].text);
const dbRow3 = row(3);
check("read: invoice 3 through MCP equals the database row", !read.isError && readBody.id === 3 && readBody.status === dbRow3.status && readBody.note === dbRow3.note, { tool: readBody, database: dbRow3, evidence: read._meta });

const list = await call(reader, "invoicedesk_invoices", { status: "needs_review" });
const listBody = JSON.parse(list.content[0].text);
const dbNeedsReview = db.prepare("SELECT id FROM invoices WHERE status = 'needs_review' ORDER BY id").all().map(r => r.id);
check("read: list filtered by status equals the database", !list.isError && JSON.stringify(listBody.map(r => r.id).sort()) === JSON.stringify(dbNeedsReview), { tool: listBody.map(r => r.id), database: dbNeedsReview });

// 3. Forbidden: the read-only grant cannot run the write, and nothing changes.
const before1 = row(1);
const forbiddenUnlisted = await call(reader, "invoices_id_status", { id: "1", status: "approved", note: "forbidden attempt" }).catch(error => ({ thrown: String(error.message ?? error) }));
const after1 = row(1);
check("forbidden: read-only grant calling the write tool is refused", (forbiddenUnlisted.isError === true) || Boolean(forbiddenUnlisted.thrown), { result: forbiddenUnlisted });
check("forbidden: no state change in the database", JSON.stringify(before1) === JSON.stringify(after1), { before: before1, after: after1 });

// 4. Authorized write: invoice 1 processing → approved; the database records it.
const writeBefore = row(1);
const write = await call(writer, "invoices_id_status", { id: "1", status: "approved", note: "approved via MCP" });
const writeAfter = row(1);
const writeBody = write.structuredContent ?? JSON.parse(write.content[0].text);
check("write: authorized status change succeeds", !write.isError && writeBody.status === "approved", { tool: writeBody, evidence: write._meta });
check("write: database independently shows the change", writeBefore.status === "processing" && writeAfter.status === "approved" && writeAfter.note === "approved via MCP", { before: writeBefore, after: writeAfter });

// 5. Application refusals are typed errors, not successes, and change nothing.
const snapshot = JSON.stringify(all());
const conflict = await call(writer, "invoices_id_status", { id: "5", status: "rejected" });
check("conflict: illegal transition is an error with code CONFLICT", conflict.isError === true && conflict._meta?.["appboundry/error"]?.code === "CONFLICT", { result: conflict });
const missing = await call(writer, "invoices_id", { id: "999" });
check("not found: missing invoice is an error with code NOT_FOUND", missing.isError === true && missing._meta?.["appboundry/error"]?.code === "NOT_FOUND", { result: missing });
const invalid = await call(writer, "invoices_id_status", { status: "approved" });
check("invalid input: missing path parameter refused", invalid.isError === true, { result: invalid });
check("refusals: no state change in the database", JSON.stringify(all()) === snapshot, {});

await reader.close();
await writer.close();
console.log(JSON.stringify({ passed: results.filter(r => r.ok).length, failed: results.filter(r => !r.ok).length, results }, null, 2));
process.exit(results.every(r => r.ok) ? 0 : 1);
