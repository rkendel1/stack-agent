"""The model proxy: the only thing `stack-agent` talks to during a trial.

`stack-agent work` is pointed at a loopback OpenAI-compatible endpoint served here. The proxy either
forwards each request to the real provider (adding the real key, which `stack-agent` never sees) or
answers from a deterministic script. In both modes it records, per call, what the harness needs and
nothing sensitive: timing, HTTP status, provider-reported usage, a summary of the decision the reply
carries (its type, capability, input names and a project-relative path), and whether the request
carried recalled episodes or workflow guidance. It never stores a prompt or a reply's content.

It is also where faults at decision boundaries are injected: a hook can kill the work before a
reply is delivered (the decision is lost; nothing was started) or right after (the decision is
delivered and the process dies while acting on it).
"""

from __future__ import annotations

import hashlib
import json
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Callable

RECALL_MARKER = "recalled episode "
WORKFLOW_MARKER = "workflow candidate "

# One pacer per upstream model, shared by every trial this process runs: some providers allow only a
# few requests per minute per model.
_PACERS: dict[str, "Pacer"] = {}
_PACERS_LOCK = threading.Lock()


class Pacer:
    def __init__(self, min_interval_s: float):
        self.min_interval_s = min_interval_s
        self.lock = threading.Lock()
        self.next_at = 0.0

    def wait(self) -> float:
        with self.lock:
            now = time.monotonic()
            delay = max(0.0, self.next_at - now)
            self.next_at = max(now, self.next_at) + self.min_interval_s
        if delay:
            time.sleep(delay)
        return delay

    def push_back(self, seconds: float) -> None:
        with self.lock:
            self.next_at = max(self.next_at, time.monotonic() + seconds)


def pacer_for(model: str, min_interval_s: float) -> Pacer:
    with _PACERS_LOCK:
        p = _PACERS.get(model)
        if p is None:
            p = _PACERS[model] = Pacer(min_interval_s)
        return p


def summarize_decision(content: str | None) -> dict:
    """What a reply decided, without its content (no text, no file content, no summary)."""
    if content is None:
        return {"parsed": False, "reason": "no content"}
    try:
        d = json.loads(content)
    except Exception:
        return {"parsed": False, "bytes": len(content.encode())}
    if not isinstance(d, dict):
        return {"parsed": False, "bytes": len(content.encode())}
    inputs = d.get("inputs") if isinstance(d.get("inputs"), dict) else {}
    path = inputs.get("path")
    if not (isinstance(path, str) and not path.startswith("/") and ".." not in path and len(path) < 200):
        path = None
    content_value = inputs.get("content")
    return {
        "parsed": True,
        "decision": d.get("decision") if isinstance(d.get("decision"), str) else None,
        "capability": d.get("capability") if isinstance(d.get("capability"), str) else None,
        "input_names": sorted(k for k in inputs if isinstance(k, str))[:12],
        "path": path,
        # Identifies a write's content (to find repeated writes) without keeping it.
        "content_digest": hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()[:16]
        if isinstance(content_value, str) or "with" in inputs else None,
    }


def sse_body(index: int, content: str, usage: dict) -> bytes:
    """A chat-completions event stream carrying `content` in one delta, then usage."""
    base = {"id": f"scripted-{index}", "object": "chat.completion.chunk", "model": "scripted"}
    events = [
        {**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": content}, "finish_reason": None}]},
        {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        {**base, "choices": [], "usage": usage},
    ]
    return b"".join(b"data: " + json.dumps(e).encode() + b"\n\n" for e in events) + b"data: [DONE]\n\n"


def parse_sse(payload: bytes) -> tuple[str | None, dict | None, str | None]:
    """Reassembles a streamed reply: its content, the usage it reported, its finish reason."""
    parts, usage, finish = [], None, None
    for line in payload.decode(errors="replace").splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            break
        try:
            event = json.loads(data)
        except Exception:
            continue
        if event.get("usage"):
            usage = event["usage"]
        for choice in event.get("choices") or []:
            delta = choice.get("delta") or {}
            if isinstance(delta.get("content"), str):
                parts.append(delta["content"])
            if choice.get("finish_reason"):
                finish = choice["finish_reason"]
    return ("".join(parts) if parts else None), usage, finish


class ModelProxy:
    """A loopback endpoint for one trial (or one seeding work)."""

    def __init__(self, *, mode: str, upstream: str | None = None, key: str | None = None,
                 model: str | None = None, min_interval_s: float = 0.0,
                 max_rate_retries: int = 8, script=None,
                 timeout_s: float = 300.0):
        assert mode in ("forward", "scripted")
        self.mode = mode
        self.upstream = upstream
        self.key = key
        self.model = model
        self.pacer = pacer_for(model or "scripted", min_interval_s) if min_interval_s else None
        self.max_rate_retries = max_rate_retries
        self.script = script
        self.timeout_s = timeout_s
        self.calls: list[dict] = []
        self.lock = threading.Lock()
        # (call index, decision summary) -> None | "kill_before" | "kill_after"
        self.hook: Callable[[int, dict], str | None] | None = None
        self.on_kill: Callable[[], None] | None = None
        self.killed_at: dict | None = None
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self._server.daemon_threads = True
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}/v1/chat/completions"

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self._server.shutdown()
        self._server.server_close()

    # -- one call -----------------------------------------------------------------------------------

    def _complete(self, body: bytes, index: int) -> tuple[int, bytes, dict]:
        """Returns (status, response body, record fields)."""
        try:
            request = json.loads(body)
        except Exception:
            request = {}
        rec: dict = {"request_bytes": len(body)}
        text = "\n".join(
            m.get("content", "") if isinstance(m.get("content"), str) else ""
            for m in request.get("messages", []) if isinstance(m, dict)
        )
        rec["markers"] = {"recall": RECALL_MARKER in text, "workflow": WORKFLOW_MARKER in text}
        stream = bool(request.get("stream"))
        rec["stream"] = stream
        if self.mode == "scripted":
            content = self.script(request, index)
            usage = {"prompt_tokens": max(1, len(body) // 4), "completion_tokens": max(1, len(content) // 4),
                     "total_tokens": max(2, (len(body) + len(content)) // 4)}
            rec.update(upstream_status=None, usage=usage, finish_reason="stop",
                       decision=summarize_decision(content))
            if stream:
                return 200, sse_body(index, content, usage), rec
            out = {
                "id": f"scripted-{index}", "object": "chat.completion", "model": "scripted",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": content}}],
                "usage": usage,
            }
            return 200, json.dumps(out).encode(), rec
        retries, paced = 0, 0.0
        while True:
            if self.pacer:
                paced += self.pacer.wait()
            req = urllib.request.Request(self.upstream, data=body, method="POST", headers={
                "content-type": "application/json", "authorization": f"Bearer {self.key}"})
            try:
                with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
                    status, payload = resp.status, resp.read()
            except urllib.error.HTTPError as e:
                status, payload = e.code, e.read()
                if status == 429 and retries < self.max_rate_retries:
                    retries += 1
                    wait = float(e.headers.get("retry-after") or 12)
                    if self.pacer:
                        self.pacer.push_back(wait)
                    else:
                        time.sleep(wait)
                    continue
            except Exception as e:  # network failure: the provider never answered
                rec.update(upstream_status=None, transport_error=type(e).__name__,
                           rate_retries=retries, paced_ms=int(paced * 1000), decision=None)
                return 502, json.dumps({"error": {"message": "upstream unreachable"}}).encode(), rec
            break
        rec.update(upstream_status=status, rate_retries=retries, paced_ms=int(paced * 1000))
        if status == 200:
            try:
                if stream or payload.lstrip().startswith(b"data:"):
                    content, usage, finish = parse_sse(payload)
                else:
                    data = json.loads(payload)
                    choice = (data.get("choices") or [{}])[0]
                    content, usage, finish = ((choice.get("message") or {}).get("content"),
                                              data.get("usage"), choice.get("finish_reason"))
                rec["usage"] = usage
                rec["finish_reason"] = finish
                rec["decision"] = summarize_decision(content)
            except Exception:
                rec["decision"] = {"parsed": False, "reason": "unreadable provider body"}
        else:
            rec["decision"] = None
        return status, payload, rec

    def _handler(self):
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                length = int(self.headers.get("content-length") or 0)
                body = self.rfile.read(length)
                with proxy.lock:
                    index = len(proxy.calls) + 1
                    proxy.calls.append({"call": index, "pending": True})
                started = time.monotonic()
                status, payload, rec = proxy._complete(body, index)
                rec.update(call=index, status=status, latency_ms=int((time.monotonic() - started) * 1000))
                action = proxy.hook(index, rec.get("decision") or {}) if proxy.hook else None
                with proxy.lock:
                    proxy.calls[index - 1] = rec
                if action == "kill_before":
                    rec["delivered"] = False
                    proxy.killed_at = {"call": index, "when": "before_reply"}
                    if proxy.on_kill:
                        proxy.on_kill()
                    return
                self.send_response(status)
                self.send_header("content-type", "text/event-stream" if rec.get("stream") and status == 200
                                 else "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()
                if proxy.mode == "scripted" and hasattr(proxy.script, "delivered"):
                    proxy.script.delivered()
                if action == "kill_after":
                    proxy.killed_at = {"call": index, "when": "after_reply"}
                    if proxy.on_kill:
                        proxy.on_kill()

        return Handler
