"""One process of E3 (handoff + approval) / E2 W5, W6.

Graph: triage --Command(goto)--> specialist --(charge proposed)--> approval[interrupt()] --> tools --> specialist
usage: e3_child.py <db> <thread> start|resume|approve|inspect
env: KILL_AT=W5|W6, FAIL_SPECIALIST=1 (raise once per process), RETRY=1 (RetryPolicy on specialist)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import json
import sqlite3
from typing import Literal

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from langgraph.graph import END, START, MessagesState, StateGraph
from langgraph.prebuilt import ToolNode
from langgraph.runtime import Runtime
from langgraph.types import Command, RetryPolicy, interrupt

import common
from common import KILL_AT, TOOLS, KillingSqliteSaver, ScriptedChatModel, _append_fsync, _last_tool, die, tc

NODES = os.environ.get("NODES", "")  # node-entry log


class State(MessagesState):
    active_agent: str


def script():
    return [
        lambda ms: tc("transfer_to_specialist", {"reason": "billing request"}, 0),
        lambda ms: tc("lookup", {"key": "price"}, 1),
        lambda ms: tc("charge", {"account": "acct-7", "amount": int(_last_tool(ms, "lookup"))}, 2),
        lambda ms: AIMessage(content=f"Charged acct-7 the amount {_last_tool(ms, 'lookup')}."),
    ]


_fail = {"left": 1 if os.environ.get("FAIL_SPECIALIST") else 0}


def build(model, saver):
    def log(name, state, runtime):
        ei = runtime.execution_info
        _append_fsync(NODES, {"node": name, "pid": os.getpid(), "task_id": ei.task_id,
                              "checkpoint_id": ei.checkpoint_id, "thread_id": ei.thread_id,
                              "attempt": ei.node_attempt, "n_messages_seen": len(state["messages"]),
                              "active_agent": state.get("active_agent")})

    def triage(state: State, runtime: Runtime) -> Command[Literal["specialist"]]:
        log("triage", state, runtime)
        ai = model.invoke(state["messages"])
        call = ai.tool_calls[0]
        return Command(goto="specialist", update={
            "messages": [ai, ToolMessage("Transferred to specialist", name="transfer_to_specialist",
                                         tool_call_id=call["id"])],
            "active_agent": "specialist"})

    def specialist(state: State, runtime: Runtime) -> Command[Literal["approval", "tools", "__end__"]]:
        log("specialist", state, runtime)
        if _fail["left"]:
            _fail["left"] -= 1
            raise RuntimeError("specialist failed (injected)")
        ai = model.invoke(state["messages"])
        if not ai.tool_calls:
            return Command(goto=END, update={"messages": [ai]})
        goto = "approval" if any(c["name"] == "charge" for c in ai.tool_calls) else "tools"
        return Command(goto=goto, update={"messages": [ai]})

    def approval(state: State, runtime: Runtime) -> Command[Literal["tools", "specialist"]]:
        log("approval", state, runtime)
        call = next(c for c in state["messages"][-1].tool_calls if c["name"] == "charge")
        decision = interrupt({"action": "charge", "args": call["args"], "tool_call_id": call["id"]})
        _append_fsync(NODES, {"node": "approval:decision", "pid": os.getpid(), "decision": decision})
        if decision.get("approve"):
            return Command(goto="tools")
        return Command(goto="specialist", update={"messages": [
            ToolMessage("charge rejected by human", name="charge", tool_call_id=call["id"])]})

    g = StateGraph(State)
    g.add_node("triage", triage)
    retry = RetryPolicy(max_attempts=2, initial_interval=0.01, jitter=False, retry_on=RuntimeError) \
        if os.environ.get("RETRY") else None
    g.add_node("specialist", specialist, retry_policy=retry)
    g.add_node("approval", approval)
    g.add_node("tools", ToolNode(TOOLS))
    g.add_edge(START, "triage")
    g.add_edge("tools", "specialist")
    return g.compile(checkpointer=saver)


def summarize(snap):
    msgs = snap.values.get("messages", []) if snap.values else []
    return {
        "next": list(snap.next),
        "checkpoint_id": snap.config["configurable"].get("checkpoint_id") if snap.config else None,
        "step": (snap.metadata or {}).get("step"),
        "active_agent": (snap.values or {}).get("active_agent"),
        "messages": [f"{type(m).__name__}:{getattr(m, 'name', None) or ','.join(c['name'] for c in getattr(m, 'tool_calls', []) or [])}" for m in msgs],
        "interrupts": [{"id": i.id, "value": i.value} for i in snap.interrupts],
        "task_errors": [{"name": t.name, "error": str(t.error)} for t in snap.tasks if t.error],
    }


def main():
    db, thread, mode = sys.argv[1:4]
    saver = KillingSqliteSaver(sqlite3.connect(db, check_same_thread=False))
    cfg = {"configurable": {"thread_id": thread}}
    model = ScriptedChatModel(script=script(), counter={})
    graph = build(model, saver)
    before = graph.get_state(cfg)
    model.offset = sum(isinstance(m, AIMessage) for m in (before.values or {}).get("messages", []))
    out = {"pid": os.getpid(), "mode": mode, "offset": model.offset, "before": summarize(before)}
    if mode != "inspect":
        inp = {"messages": [HumanMessage("Charge acct-7 the price, then report.")], "active_agent": "triage"} \
            if mode == "start" else (None if mode == "resume" else Command(resume={"approve": True}))
        try:
            res = graph.invoke(inp, cfg, durability="sync")
            out["returned"] = True
            out["interrupt_returned"] = [str(i) for i in (res or {}).get("__interrupt__", [])]
            out["last_message"] = str(res["messages"][-1].content) if res and res.get("messages") else None
        except Exception as e:  # noqa: BLE001
            out["error"] = f"{type(e).__name__}: {e}"
        out["after"] = summarize(graph.get_state(cfg))
    print(json.dumps(out, default=str), flush=True)
    if KILL_AT == "W6" and mode == "start" and out.get("after", {}).get("interrupts"):
        die("W6:suspended-awaiting-approval")


if __name__ == "__main__":
    main()
