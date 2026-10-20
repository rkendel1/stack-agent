"""Shared fixture for the LangGraph harnesses (protocol.md "Shared fixture").

Uses only LangGraph / langchain-core public API:
  * ScriptedChatModel: a langchain_core BaseChatModel subclass that replays a
    deterministic script. Chosen over langchain's GenericFakeChatModel because
    that class cannot emit tool calls from bind_tools(); a custom BaseChatModel
    goes through exactly the same interface a real provider adapter uses.
  * lookup / charge: langchain_core @tool functions run by langgraph.prebuilt.ToolNode.
  * KillingSqliteSaver: SqliteSaver subclass whose overrides only add SIGKILL
    hooks at exact points of the checkpointer write path (W3, W4, W4b, W5).
Kill points are reached deterministically by env var KILL_AT; never by timing.
"""
from __future__ import annotations

import json
import os
import signal
import sqlite3
import time
from contextlib import contextmanager
from typing import Any, Callable, Iterator

from langchain_core.language_models.chat_models import BaseChatModel
from langchain_core.messages import AIMessage, BaseMessage, ToolMessage
from langchain_core.outputs import ChatGeneration, ChatResult
from langchain_core.tools import tool
from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.prebuilt import ToolRuntime

KILL_AT = os.environ.get("KILL_AT", "")
LEDGER = os.environ.get("LEDGER", "")
CALLS = os.environ.get("CALLS", "")  # model-call log (not ground truth, diagnostics)
EVENTS = os.environ.get("EVENTS", "")  # kill-point log


def _append_fsync(path: str, rec: dict) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, default=str) + "\n")
        f.flush()
        os.fsync(f.fileno())


def die(point: str) -> None:
    """SIGKILL this process at a named kill point (never a graceful signal)."""
    _append_fsync(EVENTS, {"killed_at": point, "pid": os.getpid(), "t": time.time()})
    os.kill(os.getpid(), signal.SIGKILL)


# --------------------------------------------------------------------------- tools
_invocations = {"n": 0}


def _ledger(tool_name: str, args: dict, runtime: ToolRuntime | None) -> None:
    _invocations["n"] += 1
    ei = getattr(runtime, "execution_info", None) if runtime else None
    _append_fsync(
        LEDGER,
        {
            "tool": tool_name,
            "args": args,
            "invocation": _invocations["n"],
            "pid": os.getpid(),
            "t": time.time(),
            # What LangGraph exposes that *could* serve as an idempotency key.
            # LangGraph does not forward a key on its own; we record whether a
            # stable one is derivable (task_id + tool_call_id) across re-execution.
            "task_id": getattr(ei, "task_id", None),
            "checkpoint_id": getattr(ei, "checkpoint_id", None),
            "tool_call_id": getattr(runtime, "tool_call_id", None) if runtime else None,
        },
    )


@tool
def lookup(key: str, runtime: ToolRuntime) -> str:
    """Look up a value by key."""
    _ledger("lookup", {"key": key}, runtime)
    return "42"


@tool
def charge(account: str, amount: int, runtime: ToolRuntime) -> str:
    """Charge an account a given amount."""
    if KILL_AT == "W1":
        die("W1:before-charge")
    hold = os.environ.get("HOLD_IN_CHARGE")
    if hold:  # barrier (file-based, not timing): announce entry, wait for release
        open(hold + ".entered", "w").close()
        while not os.path.exists(hold + ".release"):
            time.sleep(0.01)
    _ledger("charge", {"account": account, "amount": amount}, runtime)
    if KILL_AT == "W2":
        die("W2:inside-charge-after-ledger-fsync")
    return f"charged {account} {amount}"


TOOLS = [lookup, charge]


# --------------------------------------------------------------------- fake model
def tc(name: str, args: dict, i: int) -> AIMessage:
    return AIMessage(content="", tool_calls=[{"name": name, "args": args, "id": f"call_{i}"}])


def _last_tool(messages: list[BaseMessage], name: str) -> str | None:
    for m in reversed(messages):
        if isinstance(m, ToolMessage) and m.name == name:
            return str(m.content)
    return None


def e1_script() -> list[Callable[[list[BaseMessage]], AIMessage]]:
    return [
        lambda ms: tc("lookup", {"key": "price"}, 1),
        lambda ms: tc("charge", {"account": "acct-7", "amount": int(_last_tool(ms, "lookup"))}, 2),
        lambda ms: AIMessage(content=f"Charged acct-7 the amount {_last_tool(ms, 'lookup')}."),
    ]


def e1_invalid_script():
    return [
        lambda ms: tc("lookup", {"key": "price"}, 1),
        lambda ms: tc("charge", {"account": "acct-7"}, 2),  # omits required `amount`
        lambda ms: tc("charge", {"account": "acct-7", "amount": int(_last_tool(ms, "lookup"))}, 3),
        lambda ms: AIMessage(content=f"Charged acct-7 the amount {_last_tool(ms, 'lookup')}."),
    ]


class ScriptedChatModel(BaseChatModel):
    """Reply index = replies already recorded in restored state (offset) + calls in this process."""

    script: list = []
    offset: int = 0
    counter: dict = {}

    @property
    def _llm_type(self) -> str:
        return "scripted-fake"

    def bind_tools(self, tools: Any, **kwargs: Any) -> "ScriptedChatModel":  # tools ignored
        return self

    def _generate(self, messages, stop=None, run_manager=None, **kwargs) -> ChatResult:
        n = self.counter.setdefault("calls", 0)
        idx = self.offset + n
        self.counter["calls"] = n + 1
        _append_fsync(CALLS, {"pid": os.getpid(), "index": idx, "t": time.time()})
        if KILL_AT == "W3b" and idx == 2:
            die("W3b:start-of-model-call-3-after-tools-checkpoint")
        msg = self.script[idx](list(messages))
        return ChatResult(generations=[ChatGeneration(message=msg)])


# ---------------------------------------------------------------- checkpointer
def _has_tool_msg(value: Any, name: str) -> bool:
    vals = value if isinstance(value, list) else [value]
    return any(isinstance(v, ToolMessage) and v.name == name for v in vals)


class KillingSqliteSaver(SqliteSaver):
    """SqliteSaver + kill hooks. Behaviour is unchanged unless KILL_AT names a hook."""

    _kill_before_commit: str | None = None
    _w4_done = False

    @contextmanager
    def cursor(self, transaction: bool = True) -> Iterator[sqlite3.Cursor]:
        with self.lock:
            self.setup()
            cur = self.conn.cursor()
            try:
                yield cur
            finally:
                if transaction:
                    if self._kill_before_commit:
                        # statements executed, transaction NOT committed
                        die(self._kill_before_commit)
                    self.conn.commit()
                cur.close()

    def put_writes(self, config, writes, task_id, task_path=""):
        charge_w = any(c == "messages" and _has_tool_msg(v, "charge") for c, v in writes)
        handoff_w = any(c == "messages" and _has_tool_msg(v, "transfer_to_specialist") for c, v in writes)
        if KILL_AT == "W4b" and charge_w:
            self._kill_before_commit = "W4b:mid-put_writes-of-charge-result-uncommitted"
        super().put_writes(config, writes, task_id, task_path)
        if KILL_AT == "W3" and charge_w:
            die("W3:after-charge-result-committed-as-pending-write")
        if KILL_AT == "W5" and handoff_w:
            die("W5:after-handoff-committed-as-pending-write-before-specialist")

    def put(self, config, checkpoint, metadata, new_versions):
        msgs = checkpoint.get("channel_values", {}).get("messages", [])
        if KILL_AT == "W4" and not self._w4_done and _has_tool_msg(msgs, "charge"):
            self._w4_done = True
            self._kill_before_commit = "W4:mid-put-of-checkpoint-containing-charge-uncommitted"
        return super().put(config, checkpoint, metadata, new_versions)


def read_jsonl(path: str) -> list[dict]:
    if not path or not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]
