import { createServer } from 'node:http';
import { appendFileSync, readFileSync } from 'node:fs';
// Offline stand-in for the Anthropic Messages API: replays one scripted decision per call and logs every request body.
// Usage: node fake-anthropic.mjs PORT REQUEST_LOG SCRIPT_FILE (one decision JSON per line).
const [port, log, script] = process.argv.slice(2);
const FIX = readFileSync(script, 'utf8').trim().split('\n');
let n = 0;
createServer(async (req, res) => {
  const chunks = []; for await (const c of req) chunks.push(c);
  const raw = Buffer.concat(chunks).toString(); appendFileSync(log, raw + '\n');
  const body = JSON.parse(raw); const text = FIX[Math.min(n++, FIX.length - 1)];
  const usage = { input_tokens: 10, output_tokens: 1 };
  const ev = (e, d) => res.write(`event: ${e}\ndata: ${JSON.stringify(d)}\n\n`);
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  ev('message_start', { type: 'message_start', message: { id: 'm', type: 'message', role: 'assistant', model: body.model, content: [], stop_reason: null, usage } });
  ev('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
  ev('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } });
  ev('content_block_stop', { type: 'content_block_stop', index: 0 });
  ev('message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 5 } });
  ev('message_stop', { type: 'message_stop' }); res.end();
}).listen(Number(port), '127.0.0.1');
