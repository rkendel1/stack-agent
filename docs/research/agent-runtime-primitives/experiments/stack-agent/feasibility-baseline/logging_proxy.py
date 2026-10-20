#!/usr/bin/env python3
"""A logging pass-through proxy in front of the local inference endpoint (stdlib only).

It forwards every request to UPSTREAM unchanged and writes one JSON line per exchange to --log:
the request's shape (message roles and sizes, whether a response format or tools are requested, the
token limit, the temperature, the model), a digest and the size of each message, and from the
response the finish reason, the usage, the size of the content and of the reasoning, and the time.
It never alters a request or a response, and it only ever talks to the endpoint it is pointed at.
Message text is not logged, only its size and SHA-256, except with --full (used once, for the audit).
"""
import argparse
import hashlib
import http.client
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

LOCK = threading.Lock()


def digest(text):
    return "sha256:" + hashlib.sha256(text.encode()).hexdigest()[:16]


def message_text(m):
    c = m.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return "".join(p.get("text", "") for p in c if isinstance(p, dict))
    return ""


def summarize_stream(raw):
    """finish reason, usage and sizes from an SSE body (or a plain JSON body)."""
    out = {"finish_reason": None, "usage": None, "content_chars": 0, "reasoning_chars": 0, "chunks": 0}
    text = raw.decode("utf-8", "replace")
    events = []
    if text.lstrip().startswith("{"):
        try:
            events = [json.loads(text)]
        except ValueError:
            pass
    else:
        for line in text.splitlines():
            if line.startswith("data:") and "[DONE]" not in line:
                try:
                    events.append(json.loads(line[5:]))
                except ValueError:
                    pass
    for e in events:
        out["chunks"] += 1
        if e.get("usage"):
            out["usage"] = e["usage"]
        for ch in e.get("choices") or []:
            if ch.get("finish_reason"):
                out["finish_reason"] = ch["finish_reason"]
            d = ch.get("delta") or ch.get("message") or {}
            out["content_chars"] += len(d.get("content") or "")
            out["reasoning_chars"] += len(d.get("reasoning_content") or d.get("reasoning") or "")
    return out


def make_handler(upstream, log, full):
    up = urlparse(upstream)

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("content-length", "0")))
            t0 = time.perf_counter()
            try:
                req = json.loads(body)
            except ValueError:
                req = {}
            conn = http.client.HTTPConnection(up.hostname, up.port, timeout=600)
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "connection")}
            conn.request("POST", self.path, body=body, headers=headers)
            r = conn.getresponse()
            raw = b""
            self.send_response(r.status)
            for k, v in r.getheaders():
                if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                    self.send_header(k, v)
            self.send_header("connection", "close")
            self.end_headers()
            try:
                while True:
                    chunk = r.read(4096)
                    if not chunk:
                        break
                    raw += chunk
                    self.wfile.write(chunk)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            finally:
                conn.close()
            self.close_connection = True
            msgs = req.get("messages", [])
            rec = {
                "t_start": round(t0, 3), "seconds": round(time.perf_counter() - t0, 2), "status": r.status,
                "model": req.get("model"), "stream": req.get("stream"),
                "max_tokens": req.get("max_tokens") or req.get("max_completion_tokens"),
                "temperature": req.get("temperature"),
                "response_format": (req.get("response_format") or {}).get("type"),
                "tools": len(req.get("tools") or []),
                "messages": [{"role": m.get("role"), "chars": len(message_text(m)), "sha": digest(message_text(m))} for m in msgs],
                "response": summarize_stream(raw),
            }
            if full:
                rec["messages_full"] = [{"role": m.get("role"), "text": message_text(m)} for m in msgs]
                rec["response_text"] = raw.decode("utf-8", "replace")[:20000]
            with LOCK, open(log, "a") as f:
                f.write(json.dumps(rec) + "\n")

    return H


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", default="http://localhost:1235")
    ap.add_argument("--port-file", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--full", action="store_true")
    a = ap.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(a.upstream, a.log, a.full))
    open(a.port_file + ".tmp", "w").write(str(srv.server_address[1]))
    import os
    os.replace(a.port_file + ".tmp", a.port_file)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
