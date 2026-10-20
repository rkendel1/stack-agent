#!/usr/bin/env python3
"""Scripted OpenAI-compatible chat-completions endpoint for `stack-agent work` (stdlib only).

The reply is chosen from what the request shows, not from a global counter, so a restarted
stack-agent process gets a sensible decision for the state it is in:

  * no successful project.read observation in the request  -> request project.read src/lib.rs
    ("lookup")
  * a project.read observation, no pax.test observation      -> request pax.test ("charge")
  * a pax.test observation already present                   -> block (never expected: Chip's own
    local policy completes verify work as soon as pax.test passed)

Replies stream as SSE exactly like crates/stack-agent/tests/support/gated.rs.

Kill hooks (deterministic, on request content, never on time):
  --kill-on ask_test : when asked the decision that would be answered with pax.test (W1a), SIGKILL
                       the stack-agent pid read from --pidfile, before replying.
Variants:
  --invalid read_missing_path : the first "lookup" reply omits the required `path` input.
  --invalid test_extra_input  : the first "charge" reply passes an input pax.test does not take.

Every request is appended to --log as one JSON line (phase chosen, message sizes, flags).
"""
import argparse
import json
import os
import signal
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

READ = {"decision": "request_capability", "capability": "project.read", "inputs": {"path": "src/lib.rs"}}
READ_BAD = {"decision": "request_capability", "capability": "project.read", "inputs": {}}
TEST = {"decision": "request_capability", "capability": "pax.test"}
TEST_BAD = {"decision": "request_capability", "capability": "pax.test", "inputs": {"extra": "x"}}
BLOCK = {"decision": "block", "reason": "scripted: pax.test already observed"}

STATE = {"invalid_used": False, "lock": threading.Lock(), "n": 0}


def text_of(body):
    parts = []
    for m in body.get("messages", []):
        c = m.get("content")
        if isinstance(c, str):
            parts.append(c)
        elif isinstance(c, list):
            parts.extend(p.get("text", "") for p in c if isinstance(p, dict))
    return "\n".join(parts)


def phase(text, markers):
    """What the request shows has been observed (markers are configurable for robustness)."""
    if markers["test"] in text:
        return "after_test"
    if markers["read"] in text:
        return "ask_test"
    return "ask_read"


def sse(decision_text):
    ev = lambda o: "data: " + json.dumps(o) + "\n\n"
    return (
        ev({"id": "m", "choices": [{"index": 0, "delta": {"role": "assistant", "content": decision_text}}]})
        + ev({"id": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        + ev({"id": "m", "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}})
        + "data: [DONE]\n\n"
    )


def make_handler(args):
    markers = {"read": args.read_marker, "test": args.test_marker}

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def do_POST(self):
            n = int(self.headers.get("content-length", "0"))
            raw = self.rfile.read(n)
            body = json.loads(raw)
            text = text_of(body)
            p = phase(text, markers)
            with STATE["lock"]:
                STATE["n"] += 1
                idx = STATE["n"]
                if p == "ask_read":
                    d = READ
                    if args.invalid == "read_missing_path" and not STATE["invalid_used"]:
                        d, STATE["invalid_used"] = READ_BAD, True
                elif p == "ask_test":
                    d = TEST
                    if args.invalid == "test_extra_input" and not STATE["invalid_used"]:
                        d, STATE["invalid_used"] = TEST_BAD, True
                else:
                    d = BLOCK
            rec = {"i": idx, "pid": os.getpid(), "phase": p, "reply": d, "messages": len(body.get("messages", [])), "chars": len(text)}
            if args.dump:
                with open(args.dump, "a") as f:
                    f.write(json.dumps(body) + "\n")
            if args.kill_on and p == args.kill_on:
                target = int(open(args.pidfile).read().strip())
                rec["killed"] = target
                with open(args.log, "a") as f:
                    f.write(json.dumps(rec) + "\n")
                    f.flush()
                    os.fsync(f.fileno())
                os.kill(target, signal.SIGKILL)
                self.close_connection = True
                return
            with open(args.log, "a") as f:
                f.write(json.dumps(rec) + "\n")
            payload = sse(json.dumps(d)).encode()
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
    ap.add_argument("--pidfile")
    ap.add_argument("--kill-on", choices=["ask_read", "ask_test", "after_test"])
    ap.add_argument("--invalid", choices=["read_missing_path", "test_extra_input"])
    ap.add_argument("--dump")
    ap.add_argument("--read-marker", default="pub fn one()")
    ap.add_argument("--test-marker", default="pax.execution-result.v1")
    args = ap.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(args))
    tmp = args.port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(srv.server_address[1]))
    os.replace(tmp, args.port_file)
    signal.signal(signal.SIGTERM, lambda *a: sys.exit(0))
    srv.serve_forever()


if __name__ == "__main__":
    main()
