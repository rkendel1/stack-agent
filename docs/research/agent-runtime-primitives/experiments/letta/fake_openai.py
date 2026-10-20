"""Scripted fake OpenAI-compatible Chat Completions server (stdlib only).

The reply is chosen from the conversation the client sends, i.e. from the
number of tool results already recorded (in this process or in restored state):
  0 results            -> call Bash(lookup)
  lookup result only   -> call Bash(charge acct-7 <price>)
  charge result        -> final answer
MODE=omit_arg makes the first reply omit the required `command` argument.
Every request is appended to $FAKE_LOG as one JSON line.
"""
import json, os, signal, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOLPY = os.environ["FAKE_TOOLPY"]
LOG = os.environ.get("FAKE_LOG", "fake_requests.jsonl")
MODE = os.environ.get("MODE", "")
PY = sys.executable

def tool_results(messages):
    out = []
    for m in messages:
        if m.get("role") == "tool":
            c = m.get("content")
            if isinstance(c, list):
                c = " ".join(p.get("text", "") for p in c if isinstance(p, dict))
            out.append(str(c))
    return out

def decide(body):
    res = tool_results(body.get("messages", []))
    joined = "\n".join(res)
    if "CHARGED" in joined:
        return {"text": "Charged acct-7 the amount 42."}
    if "PRICE=" in joined:
        price = joined.split("PRICE=")[1].split()[0].strip()
        return {"tool": {"command": f"{PY} -I {TOOLPY} charge acct-7 {price}", "description": "charge"}}
    if MODE == "omit_arg" and not res:
        return {"tool": {"description": "lookup without command"}}
    return {"tool": {"command": f"{PY} -I {TOOLPY} lookup price", "description": "lookup"}}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _json(self, obj, code=200):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json({"object": "list", "data": [{"id": "fake", "object": "model", "owned_by": "harness"}]})
        self._json({"ok": True})
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self._json({"error": "unsupported"}, 404)
        tools = [t.get("function", {}).get("name") for t in body.get("tools", [])]
        d = decide(body)
        # W3: the request carries the charge result, so Letta has recorded it
        # in its transcript; SIGKILL Letta before the model replies (once).
        flag = os.environ.get("KILL_FLAG")
        if (os.environ.get("KILL_AT") == "W3" and flag and not os.path.exists(flag)
                and "CHARGED" in "\n".join(tool_results(body.get("messages", [])))):
            open(flag, "w").write("W3")
            os.killpg(int(open(os.environ["LETTA_PGID_FILE"]).read()), signal.SIGKILL)
            return
        with open(LOG, "a") as f:
            f.write(json.dumps({"t": time.time(), "n_messages": len(body.get("messages", [])),
                                "tools": tools, "decision": d, "stream": body.get("stream")}) + "\n")
        call_id = f"call_{int(time.time()*1e6)}"
        if "tool" in d:
            delta = {"role": "assistant", "content": None, "tool_calls": [{"index": 0, "id": call_id, "type": "function",
                     "function": {"name": "Bash", "arguments": json.dumps(d["tool"])}}]}
            finish = "tool_calls"
        else:
            delta = {"role": "assistant", "content": d["text"]}
            finish = "stop"
        usage = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        if body.get("stream"):
            self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
            for chunk in ({"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}], "usage": usage}):
                chunk.update({"id": "x", "object": "chat.completion.chunk", "created": int(time.time()), "model": "fake"})
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode()); self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
        else:
            msg = dict(delta)
            self._json({"id": "x", "object": "chat.completion", "created": int(time.time()), "model": "fake",
                        "choices": [{"index": 0, "message": msg, "finish_reason": finish}], "usage": usage})

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
