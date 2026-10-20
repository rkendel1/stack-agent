"""E1 (minimal tool use + invalid-args run) and E5 (overhead: InMemorySaver vs SqliteSaver, N=50).

usage: python -I e1_e5.py <workdir>   -> writes <workdir>/e1.json, <workdir>/e5.json
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import json
import platform
import sqlite3
import statistics
import subprocess
import time
from importlib import metadata

work = sys.argv[1]
os.makedirs(work, exist_ok=True)
os.environ["LEDGER"] = os.path.join(work, "e1_ledger.jsonl")
os.environ.pop("KILL_AT", None)

import common  # noqa: E402

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage  # noqa: E402
from langgraph.checkpoint.memory import InMemorySaver  # noqa: E402
from langgraph.checkpoint.sqlite import SqliteSaver  # noqa: E402

from agent_graph import GOAL, build  # noqa: E402


def run_once(script, saver, thread, ledger, durability="sync"):
    common.LEDGER = ledger
    if os.path.exists(ledger):
        os.remove(ledger)
    model = common.ScriptedChatModel(script=script, counter={})
    g = build(model, saver)
    t0 = time.perf_counter()
    res = g.invoke({"messages": [HumanMessage(GOAL)]}, {"configurable": {"thread_id": thread}},
                   **({"durability": durability} if saver is not None else {}))
    wall = (time.perf_counter() - t0) * 1000
    led = common.read_jsonl(ledger)
    return res, led, model.counter["calls"], wall


def e1():
    out = {}
    res, led, calls, wall = run_once(common.e1_script(), InMemorySaver(), "e1", os.path.join(work, "e1_ledger.jsonl"))
    charges = [l for l in led if l["tool"] == "charge"]
    lookups = [l for l in led if l["tool"] == "lookup"]
    usage = [m.usage_metadata for m in res["messages"] if isinstance(m, AIMessage) and m.usage_metadata]
    out["success"] = len(lookups) == 1 and len(charges) == 1 and charges[0]["args"]["amount"] == 42
    out["model_calls"] = calls
    out["tool_calls"] = len(led)
    out["final_answer"] = res["messages"][-1].content
    out["retries"] = 0
    out["tokens"] = None if not usage else usage
    out["wall_ms"] = round(wall, 2)
    out["idempotency_key_forwarded"] = False
    out["derivable_key"] = {"task_id": charges[0]["task_id"], "tool_call_id": charges[0]["tool_call_id"]}
    # invalid-args run: scripted reply 2 omits required `amount`
    res2, led2, calls2, wall2 = run_once(common.e1_invalid_script(), InMemorySaver(), "e1-invalid",
                                         os.path.join(work, "e1_invalid_ledger.jsonl"))
    err_msgs = [m for m in res2["messages"] if isinstance(m, ToolMessage) and getattr(m, "status", "") == "error"]
    out["invalid_args_handled"] = {
        "error_returned_to_model_as_ToolMessage": bool(err_msgs),
        "error_text": str(err_msgs[0].content)[:300] if err_msgs else None,
        "charge_lines_in_ledger": sum(l["tool"] == "charge" for l in led2),
        "tool_body_invoked_with_bad_args": False if sum(l["tool"] == "charge" for l in led2) == 1 else True,
        "model_calls": calls2,
        "verified": sum(l["tool"] == "charge" for l in led2) == 1,
    }
    return out


def pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def bench(make_saver, label, n=50, durability="sync"):
    ledger = os.path.join(work, f"e5_{label}_ledger.jsonl")
    saver = make_saver()
    run_once(common.e1_script(), saver, f"{label}-warm", ledger, durability)  # warm-up
    ts = []
    for i in range(n):
        _, led, _, wall = run_once(common.e1_script(), saver, f"{label}-{i}", ledger, durability)
        assert sum(l["tool"] == "charge" for l in led) == 1
        ts.append(wall)
    return {"p10": round(pct(ts, 0.1), 3), "median": round(statistics.median(ts), 3), "p90": round(pct(ts, 0.9), 3)}


def import_time(reps=5):
    code = ("import time;t=time.perf_counter();import langgraph.graph, langgraph.prebuilt, "
            "langgraph.checkpoint.sqlite, langchain_core.messages;print((time.perf_counter()-t)*1000)")
    ts = [float(subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, text=True).stdout)
          for _ in range(reps)]
    return {"reps": reps, "median_ms": round(statistics.median(ts), 1), "min_ms": round(min(ts), 1), "max_ms": round(max(ts), 1)}


def e5():
    dbp = os.path.join(work, "e5.sqlite")
    for p in (dbp, dbp + "-wal", dbp + "-shm"):
        if os.path.exists(p):
            os.remove(p)
    return {
        "repetitions": 50,
        "warmup": 1,
        "task": "E1 (3 scripted model calls, 2 tool calls; ledger fsync per tool call included in all variants)",
        "no_checkpointer_ms": bench(lambda: None, "none"),
        "in_memory_ms": bench(InMemorySaver, "mem"),
        "durable_ms": bench(lambda: SqliteSaver(sqlite3.connect(dbp, check_same_thread=False)), "sqlite_sync"),
        "durable_async_ms": bench(lambda: SqliteSaver(sqlite3.connect(dbp, check_same_thread=False)), "sqlite_async",
                                  durability="async"),
        "durable_store": "SqliteSaver on local file, WAL (set by SqliteSaver.setup), durability=sync",
        "import_ms": import_time(),
    }


env = {
    "cpus": os.cpu_count(),
    "python": platform.python_version(),
    "os": platform.platform(),
    "machine": platform.machine(),
    "packages": {p: metadata.version(p) for p in ("langgraph", "langgraph-checkpoint", "langgraph-checkpoint-sqlite",
                                                  "langgraph-prebuilt", "langchain-core")},
    "sqlite": sqlite3.sqlite_version,
}
r1 = e1()
json.dump({"environment": env, "e1": r1}, open(os.path.join(work, "e1.json"), "w"), indent=1, default=str)
print(json.dumps(r1, default=str)[:1500])
r5 = e5()
json.dump({"environment": env, "e5": r5}, open(os.path.join(work, "e5.json"), "w"), indent=1)
print(json.dumps(r5))
