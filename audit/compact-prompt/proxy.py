#!/usr/bin/env python3
"""A logging pass-through proxy between stack-agent and a local OpenAI-compatible server.

It records, for every model call, what was sent and what came back, independently of Stack-Agent's
own accounting. It changes nothing: the request is forwarded byte for byte and the reply is returned
as received (it is buffered, which only matters for latency of a streamed reply's first byte).

    proxy.py --upstream http://127.0.0.1:1338 --log calls.jsonl [--port 0]

The first line printed is `listening <port>`. The path prefix before /v1/chat/completions is a free
label (the trial id) and is recorded with each call.
"""
import argparse, http.client, json, sys, time, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

args = None
lock = threading.Lock()


def sse_text(body: str):
    """The assistant text and usage of a (possibly streamed) chat completion body."""
    text, usage, finish = [], None, None
    if body.lstrip().startswith("data:"):
        for line in body.splitlines():
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if payload in ("", "[DONE]"):
                continue
            try:
                chunk = json.loads(payload)
            except ValueError:
                continue
            usage = chunk.get("usage") or usage
            for choice in chunk.get("choices", []):
                delta = choice.get("delta") or choice.get("message") or {}
                if delta.get("content"):
                    text.append(delta["content"])
                finish = choice.get("finish_reason") or finish
    else:
        try:
            obj = json.loads(body)
            usage = obj.get("usage")
            choice = obj["choices"][0]
            text.append((choice.get("message") or {}).get("content") or "")
            finish = choice.get("finish_reason")
        except (ValueError, KeyError, IndexError):
            pass
    return "".join(text), usage, finish


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(n)
        label = self.path.rsplit("/v1/", 1)[0].strip("/")
        try:
            req = json.loads(raw)
        except ValueError:
            req = {}
        up = urlparse(args.upstream)
        started = time.time()
        cls = http.client.HTTPSConnection if up.scheme == "https" else http.client.HTTPConnection
        conn = cls(up.hostname, up.port, timeout=600)
        # The upstream path is the part after the free label: /v1/chat/completions for a local server,
        # or the upstream's own base path (e.g. DeepSeek's /chat/completions) when --upstream-path is given.
        path = args.upstream_path or ("/v1/" + self.path.rsplit("/v1/", 1)[1])
        headers = {"content-type": "application/json"}
        if self.headers.get("authorization"):
            headers["authorization"] = self.headers["authorization"]  # forwarded, never logged
        conn.request("POST", path, raw, headers)
        resp = conn.getresponse()
        body = resp.read()
        latency = time.time() - started
        text, usage, finish = sse_text(body.decode("utf-8", "replace"))
        messages = req.get("messages", [])
        entry = {
            "label": label, "at": started, "latency_s": round(latency, 3), "status": resp.status,
            "model": req.get("model"), "stream": req.get("stream"),
            "response_format": (req.get("response_format") or {}).get("type"),
            "messages": [{"role": m.get("role"), "chars": len(m.get("content") or "")} for m in messages],
            "prompt_chars": sum(len(m.get("content") or "") for m in messages),
            "system_chars": sum(len(m.get("content") or "") for m in messages if m.get("role") == "system"),
            "first_system": (messages[0].get("content") if messages else None),
            "last_user": next((m.get("content") for m in reversed(messages) if m.get("role") == "user"), None),
            "temperature": req.get("temperature"), "max_tokens": req.get("max_tokens"),
            "reply": text, "usage": usage, "finish_reason": finish,
            "request_options": {k: v for k, v in req.items() if k != "messages"},
            "error_body": body.decode("utf-8", "replace")[:600] if resp.status >= 400 else None,
        }
        with lock, open(args.log, "a") as f:
            f.write(json.dumps(entry) + "\n")
        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() in ("content-type",):
                self.send_header(k, v)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    global args
    p = argparse.ArgumentParser()
    p.add_argument("--upstream", required=True)
    p.add_argument("--log", required=True)
    p.add_argument("--port", type=int, default=0)
    p.add_argument("--upstream-path", default=None, help="fixed path on the upstream, e.g. /chat/completions")
    args = p.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"listening {srv.server_address[1]}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
