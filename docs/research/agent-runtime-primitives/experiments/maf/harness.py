"""E1/E2 (W1-W4, W6) harness for Microsoft Agent Framework (Python) graph workflows.

Public API only: Executor, @handler, @response_handler, WorkflowBuilder,
FileCheckpointStorage, workflow.run(checkpoint_id=..., responses=...).
No model is called: a scripted fake model lives in the `model` executor.

Usage (one process per invocation):
  python -I harness.py run    <workdir> <window> [approval]   # first process; may SIGKILL itself
  python -I harness.py resume <workdir> <window> [approval]   # fresh process; resumes latest checkpoint
"""
from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import time
from dataclasses import dataclass, field
from typing import Any

from agent_framework import (
    Executor,
    FileCheckpointStorage,
    WorkflowBuilder,
    WorkflowContext,
    handler,
    register_checkpoint_type,
    response_handler,
)

MODE, WORKDIR, WINDOW = sys.argv[1], sys.argv[2], sys.argv[3]
APPROVAL = len(sys.argv) > 4 and sys.argv[4] == "approval"
LEDGER = os.path.join(WORKDIR, "ledger.jsonl")       # outside the checkpoint store
STORE = os.path.join(WORKDIR, "store")
MODEL_CALLS_THIS_PROCESS = 0
STATE = {"charge_returned": False}


def die(point: str) -> None:
    with open(os.path.join(WORKDIR, "killpoint.txt"), "w") as f:
        f.write(point)
        f.flush(); os.fsync(f.fileno())
    os.kill(os.getpid(), signal.SIGKILL)


def kill_if(point: str) -> None:
    if MODE == "run" and WINDOW == point:
        die(point)


def ledger_append(tool: str, args: dict, invocation: str) -> None:
    line = json.dumps({"tool": tool, "args": args, "invocation": invocation, "pid": os.getpid(), "t": time.time()})
    fd = os.open(LEDGER, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, (line + "\n").encode()); os.fsync(fd)
    finally:
        os.close(fd)


@dataclass
class Task:
    goal: str


@dataclass
class ToolCall:
    call_id: str
    tool: str
    args: dict


@dataclass
class ToolResult:
    call_id: str
    tool: str
    result: Any


@dataclass
class ApprovalRequest:
    call_id: str
    tool: str
    args: dict


for t in (Task, ToolCall, ToolResult, ApprovalRequest):
    register_checkpoint_type(t)


def fake_model(history: list[dict]) -> dict:
    """Deterministic script indexed by replies already recorded in (restored) state."""
    global MODEL_CALLS_THIS_PROCESS
    MODEL_CALLS_THIS_PROCESS += 1
    n_replies = sum(1 for h in history if h["role"] == "assistant")
    if n_replies == 0:
        return {"role": "assistant", "call": {"call_id": "c1", "tool": "lookup", "args": {"key": "price"}}}
    if n_replies == 1:
        price = next(h["result"] for h in history if h["role"] == "tool" and h["tool"] == "lookup")
        return {"role": "assistant", "call": {"call_id": "c2", "tool": "charge", "args": {"account": "acct-7", "amount": price}}}
    amount = next(h["result"]["amount"] for h in history if h["role"] == "tool" and h["tool"] == "charge")
    return {"role": "assistant", "final": f"Charged acct-7 {amount}"}


class Model(Executor):
    def __init__(self) -> None:
        super().__init__(id="model")
        self.history: list[dict] = []

    async def _step(self, ctx: WorkflowContext[ToolCall, str]) -> None:
        reply = fake_model(self.history)
        self.history.append(reply)
        if "call" in reply:
            c = reply["call"]
            await ctx.send_message(ToolCall(c["call_id"], c["tool"], c["args"]))
        else:
            await ctx.yield_output(reply["final"])

    @handler
    async def on_task(self, task: Task, ctx: WorkflowContext[ToolCall, str]) -> None:
        self.history.append({"role": "user", "content": task.goal})
        await self._step(ctx)

    @handler
    async def on_result(self, res: ToolResult, ctx: WorkflowContext[ToolCall, str]) -> None:
        if res.tool == "charge":
            kill_if("W3")  # charge returned; its superstep checkpoint has been saved
        self.history.append({"role": "tool", "tool": res.tool, "call_id": res.call_id, "result": res.result})
        await self._step(ctx)

    async def on_checkpoint_save(self) -> dict[str, Any]:
        return {"history": self.history}

    async def on_checkpoint_restore(self, state: dict[str, Any]) -> None:
        self.history = state.get("history", [])


class Tools(Executor):
    def __init__(self) -> None:
        super().__init__(id="tools")

    async def _execute(self, call: ToolCall, ctx: WorkflowContext[ToolResult]) -> None:
        if call.tool == "lookup":
            ledger_append("lookup", call.args, call.call_id)
            await ctx.send_message(ToolResult(call.call_id, "lookup", 42))
            return
        kill_if("W1")  # model chose charge; charge not yet invoked
        ledger_append("charge", call.args, call.call_id)  # no idempotency key exists in the API to forward
        kill_if("W2")  # effect happened and is fsynced; tool has not returned
        STATE["charge_returned"] = True
        await ctx.send_message(ToolResult(call.call_id, "charge", {"ok": True, "amount": call.args["amount"]}))

    @handler
    async def on_call(self, call: ToolCall, ctx: WorkflowContext[ToolResult]) -> None:
        if call.tool == "charge" and APPROVAL:
            await ctx.request_info(request_data=ApprovalRequest(call.call_id, call.tool, call.args), response_type=bool)
            return
        await self._execute(call, ctx)

    @response_handler
    async def on_approval(self, req: ApprovalRequest, approved: bool, ctx: WorkflowContext[ToolResult]) -> None:
        if approved:
            await self._execute(ToolCall(req.call_id, req.tool, req.args), ctx)
        else:
            await ctx.send_message(ToolResult(req.call_id, req.tool, {"ok": False, "amount": None}))


class KillableFileStorage(FileCheckpointStorage):
    """Public-API subclass used only as the W4 hook ('about to persist')."""

    async def save(self, checkpoint):  # type: ignore[override]
        if STATE["charge_returned"]:
            kill_if("W4")  # about to persist the checkpoint that records the charge result
        return await super().save(checkpoint)


def build(storage):
    model, tools = Model(), Tools()
    return (
        WorkflowBuilder(start_executor=model, checkpoint_storage=storage, name="e2-charge")
        .add_edge(model, tools)
        .add_edge(tools, model)
        .build()
    )


async def main() -> dict:
    os.makedirs(STORE, exist_ok=True)
    storage = KillableFileStorage(storage_path=STORE)
    wf = build(storage)
    out: dict[str, Any] = {"mode": MODE, "pid": os.getpid()}
    t0 = time.perf_counter()
    if MODE == "run":
        stream = wf.run(message=Task("Charge acct-7 the amount lookup('price') returns, then report."), stream=True)
    else:
        latest = await storage.get_latest(workflow_name="e2-charge")
        if latest is None:
            return {**out, "resumed": False, "error": "no checkpoint"}
        out["resumed_from"] = {"checkpoint_id": latest.checkpoint_id, "iteration": latest.iteration_count,
                               "pending_requests": list(latest.pending_request_info_events)}
        responses = {rid: True for rid in latest.pending_request_info_events} or None
        stream = wf.run(checkpoint_id=latest.checkpoint_id, responses=responses, stream=True) if responses \
            else wf.run(checkpoint_id=latest.checkpoint_id, stream=True)
    output, pending = None, []
    async for ev in stream:
        if ev.type == "output":
            output = ev.data
        elif ev.type == "request_info":
            pending.append(ev.request_id)
        elif ev.type == "status" and str(getattr(ev, "state", ev.data)).endswith("IDLE_WITH_PENDING_REQUESTS"):
            pass
    if MODE == "run" and WINDOW == "W6" and pending:
        die("W6")  # suspended awaiting approval; pending checkpoint already written by the runner
    out.update({"output": output, "pending": pending, "model_calls_this_process": MODEL_CALLS_THIS_PROCESS,
                "wall_ms": round((time.perf_counter() - t0) * 1000, 2)})
    latest = await storage.get_latest(workflow_name="e2-charge")
    hist = latest.state.get("_executor_state", {}).get("model", {}).get("history", []) if latest else []
    out["final_checkpoint_charge_results"] = sum(1 for h in hist if h.get("role") == "tool" and h.get("tool") == "charge")
    out["final_checkpoint_model_replies"] = sum(1 for h in hist if h.get("role") == "assistant")
    return out


if __name__ == "__main__":
    res = asyncio.run(main())
    with open(os.path.join(WORKDIR, f"{MODE}.json"), "w") as f:
        json.dump(res, f, indent=2, default=str)
    print(json.dumps(res, default=str))
