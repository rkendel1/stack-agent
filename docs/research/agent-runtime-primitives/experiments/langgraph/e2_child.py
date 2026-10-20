"""One process of the E2 crash/resume experiment.

usage: e2_child.py <db> <thread_id> start|resume|inspect <durability>
  start   : graph.invoke({"messages": [goal]}, config, durability=...)   (KILL_AT may fire)
  resume  : graph.invoke(None, config, durability=...)  -- LangGraph's documented resume
  inspect : read-only get_state(); prints what is durably recorded
Prints one JSON line on stdout.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # run with python -I

import json
import sqlite3

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from agent_graph import GOAL, build, count_ai
from common import KillingSqliteSaver, ScriptedChatModel, e1_script


def summarize(snapshot):
    msgs = snapshot.values.get("messages", []) if snapshot.values else []
    pending = []
    for t in snapshot.tasks:
        r = t.result
        if r and isinstance(r, dict):
            for m in r.get("messages", []):
                pending.append({"task": t.name, "type": type(m).__name__, "name": getattr(m, "name", None)})
        elif r is not None:
            pending.append({"task": t.name, "result": str(r)[:80]})
    return {
        "messages": [
            {"type": type(m).__name__, "name": getattr(m, "name", None),
             "tool_calls": [c["name"] for c in getattr(m, "tool_calls", []) or []],
             "content": str(m.content)[:60]}
            for m in msgs
        ],
        "next": list(snapshot.next),
        "pending_task_results": pending,
        "checkpoint_id": snapshot.config["configurable"].get("checkpoint_id") if snapshot.config else None,
        "step": (snapshot.metadata or {}).get("step"),
        "ai_replies": count_ai(msgs),
        "charge_toolmsgs": sum(1 for m in msgs if isinstance(m, ToolMessage) and m.name == "charge"),
        "lookup_toolmsgs": sum(1 for m in msgs if isinstance(m, ToolMessage) and m.name == "lookup"),
    }


def main():
    db, thread, mode, durability = sys.argv[1:5]
    conn = sqlite3.connect(db, check_same_thread=False)
    saver = KillingSqliteSaver(conn)
    config = {"configurable": {"thread_id": thread}}
    model = ScriptedChatModel(script=e1_script(), counter={})
    graph = build(model, saver)
    before = graph.get_state(config)
    model.offset = count_ai(before.values.get("messages", [])) if before.values else 0
    out = {"pid": os.getpid(), "mode": mode, "offset": model.offset}
    if mode == "inspect":
        out["state"] = summarize(before)
    else:
        try:
            inp = {"messages": [HumanMessage(GOAL)]} if mode == "start" else None
            res = graph.invoke(inp, config, durability=durability)
            out["returned"] = True
            out["final_answer"] = (res or {}).get("messages", [AIMessage("")])[-1].content if res else None
        except Exception as e:  # noqa: BLE001
            out["error"] = f"{type(e).__name__}: {e}"
        out["state"] = summarize(graph.get_state(config))
    print(json.dumps(out, default=str))


if __name__ == "__main__":
    main()
