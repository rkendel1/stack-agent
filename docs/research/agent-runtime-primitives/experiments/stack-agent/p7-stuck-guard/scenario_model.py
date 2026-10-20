#!/usr/bin/env python3
"""A scripted OpenAI-compatible chat-completions endpoint for `stack-agent work` (stdlib only).

Request n (0-based, counted over the life of this server, so a restarted stack-agent continues the
script) is answered with script[n], the last entry once the script is exhausted. It reads nothing
from the request: it is a model that has decided what it will ask for. Every request served is
appended to --log as one JSON line {"i": n, "reply": <decision text>}.

Replies stream as SSE exactly like crates/stack-agent/tests/support/gated.rs.
"""
import argparse
import json
import os
import signal
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = {"n": 0, "lock": threading.Lock()}


def sse(text):
    ev = lambda o: "data: " + json.dumps(o) + "\n\n"
    return (
        ev({"id": "m", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}}]})
        + ev({"id": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        + ev({"id": "m", "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}})
        + "data: [DONE]\n\n"
    )


def handler(script, log):
    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length", "0")))
            with STATE["lock"]:
                n = STATE["n"]
                STATE["n"] += 1
                reply = script[min(n, len(script) - 1)]
                with open(log, "a") as f:
                    f.write(json.dumps({"i": n, "reply": reply}) + "\n")
            payload = sse(reply).encode()
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload)))
            self.send_header("connection", "close")
            self.end_headers()
            self.wfile.write(payload)
            self.close_connection = True

    return H


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port-file", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--script", required=True, help="JSON file: a list of decision texts")
    args = ap.parse_args()
    script = json.load(open(args.script))
    srv = ThreadingHTTPServer(("127.0.0.1", 0), handler(script, args.log))
    tmp = args.port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(srv.server_address[1]))
    os.replace(tmp, args.port_file)
    signal.signal(signal.SIGTERM, lambda *a: sys.exit(0))
    srv.serve_forever()


if __name__ == "__main__":
    main()
