// An HTTP proxy in front of the application that appends one JSON line per request it forwards
// (method, path, body) to a log: independent evidence of which requests reached the application.
import { createServer, request as forward } from "node:http";
import { appendFileSync } from "node:fs";
const [listenPort, upstreamPort, log] = process.argv.slice(2);
createServer((req, res) => {
  const chunks = [];
  req.on("data", c => chunks.push(c));
  req.on("end", () => {
    const body = Buffer.concat(chunks);
    appendFileSync(log, JSON.stringify({ at: new Date().toISOString(), method: req.method, path: req.url, body: body.toString("utf8") || null }) + "\n");
    const upstream = forward({ host: "127.0.0.1", port: Number(upstreamPort), method: req.method, path: req.url, headers: req.headers }, answer => {
      res.writeHead(answer.statusCode ?? 502, answer.headers);
      answer.pipe(res);
    });
    upstream.on("error", () => { res.writeHead(502); res.end(); });
    upstream.end(body);
  });
}).listen(Number(listenPort), "127.0.0.1");
