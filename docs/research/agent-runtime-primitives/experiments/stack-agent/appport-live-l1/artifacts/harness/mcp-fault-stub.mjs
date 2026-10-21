// Transport fault injection in front of a real MCP endpoint. initialize and tools/list are forwarded
// unchanged; tools/call depends on the mode:
//   garbage  — answer HTTP 200 with a body that is not JSON (the call is NOT forwarded)
//   noresult — answer HTTP 200 with a JSON-RPC message that has neither result nor error (NOT forwarded)
//   closeafterlist — forward everything, and stop accepting connections once tools/list is answered,
//              so a later tools/call is refused at connect: nothing is sent
//   hangup   — forward the call to the real endpoint (so it really executes), wait for its answer,
//              then destroy the client connection without replying: an ambiguous outcome
import { createServer, request as forward } from "node:http";
import { appendFileSync } from "node:fs";
const [listenPort, target, mode, log] = process.argv.slice(2);
const url = new URL(target);
const server = createServer((req, res) => {
  const chunks = [];
  req.on("data", c => chunks.push(c));
  req.on("end", () => {
    const body = Buffer.concat(chunks);
    let message = {};
    try { message = JSON.parse(body.toString("utf8")); } catch {}
    const isCall = message.method === "tools/call";
    appendFileSync(log, JSON.stringify({ at: new Date().toISOString(), method: message.method, tool: message.params?.name ?? null, mode: isCall ? mode : "forward" }) + "\n");
    if (isCall && mode === "garbage") { res.writeHead(200, { "content-type": "application/json" }); res.end("<html>this is not JSON-RPC</html>"); return; }
    if (isCall && mode === "noresult") { res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ jsonrpc: "2.0", id: message.id })); return; }
    const upstream = forward({ host: url.hostname, port: Number(url.port), method: req.method, path: url.pathname, headers: req.headers }, answer => {
      const parts = [];
      answer.on("data", c => parts.push(c));
      answer.on("end", () => {
        if (isCall && mode === "hangup") { req.socket.destroy(); return; }
        res.writeHead(answer.statusCode ?? 502, { ...answer.headers, connection: "close" });
        res.end(Buffer.concat(parts));
        if (mode === "closeafterlist" && message.method === "tools/list") server.close();
      });
    });
    upstream.on("error", () => { res.writeHead(502); res.end(); });
    upstream.end(body);
  });
}).listen(Number(listenPort), "127.0.0.1");
