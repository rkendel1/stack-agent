"""E2 orchestrator: real SIGKILL at each window, fresh-process resume with SqliteSaver.

usage: python -I e2.py <workdir> [durability ...]   (default: sync)
Writes <workdir>/e2_<durability>.json and per-window logs under <workdir>/e2/.
"""
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import read_jsonl  # noqa: E402

WINDOWS = os.environ.get("E2_WINDOWS", "W1,W2,W3,W3b,W4,W4b").split(",")
REPS = int(os.environ.get("E2_REPS", "5"))


def run(env, *args):
    p = subprocess.run([sys.executable, "-I", os.path.join(HERE, "e2_child.py"), *args],
                       env=env, capture_output=True, text=True, timeout=120)
    out = None
    for line in p.stdout.splitlines():
        if line.startswith("{"):
            out = json.loads(line)
    return p.returncode, out, p.stderr[-2000:]


def window(work, w, durability, rep=0):
    d = os.path.join(work, "e2", f"{durability}_{w}_r{rep}")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    base = dict(os.environ, LEDGER=f"{d}/ledger.jsonl", CALLS=f"{d}/calls.jsonl", EVENTS=f"{d}/events.jsonl")
    db, th = f"{d}/state.sqlite", f"t-{w}"
    rc1, o1, e1 = run(dict(base, KILL_AT=w), db, th, "start", durability)
    killed = read_jsonl(base["EVENTS"])
    rc_i, oi, _ = run(base, db, th, "inspect", durability)
    rc2, o2, e2 = run(base, db, th, "resume", durability)
    ledger = read_jsonl(base["LEDGER"])
    calls = read_jsonl(base["CALLS"])
    charges = [l for l in ledger if l["tool"] == "charge"]
    lookups = [l for l in ledger if l["tool"] == "lookup"]
    pre = (oi or {}).get("state", {})
    post = (o2 or {}).get("state", {})
    resume_pid = (o2 or {}).get("pid")
    # progress_lost: something durably recorded before the kill was re-executed by the resume process
    rec_tools = {m["name"] for m in pre.get("messages", []) if m["type"] == "ToolMessage"}
    rec_tools |= {p["name"] for p in pre.get("pending_task_results", []) if p.get("type") == "ToolMessage"}
    redone_tools = sorted({l["tool"] for l in ledger if l["pid"] == resume_pid and l["tool"] in rec_tools})
    redone_model = [c["index"] for c in calls if c["pid"] == resume_pid and c["index"] < pre.get("ai_replies", 0)]
    finished = bool(o2 and o2.get("returned") and post.get("next") == [] and "42" in str(o2.get("final_answer")))
    consistent = post.get("charge_toolmsgs") == len(charges) and post.get("lookup_toolmsgs") == len(lookups)
    if not o2 or "error" in (o2 or {}):
        outcome = "crashed_on_resume"
    elif len(charges) > 1:
        outcome = "duplicate_effect"
    elif redone_tools or redone_model:
        outcome = "lost_progress" if not finished else "correct"
    elif finished and len(charges) == 1:
        outcome = "correct"
    elif not finished:
        outcome = "stuck"
    else:
        outcome = "lost_progress"
    rec = {
        "killed_at": killed[0]["killed_at"] if killed else None,
        "start_returncode": rc1,
        "resumed": bool(o2 and o2.get("returned") and post.get("next") == []),
        "charges_in_ledger": len(charges),
        "lookups_in_ledger": len(lookups),
        "final_state_consistent": consistent,
        "progress_lost": bool(redone_tools or redone_model),
        "redone_tools": redone_tools,
        "redone_model_call_indices": redone_model,
        "manual_intervention": "none (graph.invoke(None, config) in a fresh process)",
        "outcome": outcome,
        "pre_resume_recorded": {"messages": [f"{m['type']}:{m['name'] or ','.join(m['tool_calls'])}" for m in pre.get("messages", [])],
                                "pending_task_results": pre.get("pending_task_results"), "next": pre.get("next"),
                                "step": pre.get("step")},
        "charge_task_ids": [c["task_id"] for c in charges],
        "charge_tool_call_ids": [c["tool_call_id"] for c in charges],
        "model_calls_total": len(calls),
        "final_answer": (o2 or {}).get("final_answer"),
        "resume_error": (o2 or {}).get("error"),
    }
    if rc1 != -9:
        rec["note_start"] = f"start process exited rc={rc1} (expected -9 SIGKILL); stderr tail: {e1[-300:]}"
    with open(f"{d}/result.json", "w") as f:
        json.dump({"start": o1, "inspect": oi, "resume": o2, "resume_stderr": e2[-1500:], "result": rec}, f, indent=1, default=str)
    return rec


def main():
    work = sys.argv[1]
    modes = sys.argv[2:] or ["sync"]
    for m in modes:
        res = {}
        for w in WINDOWS:
            reps = [window(work, w, m, i) for i in range(REPS)]
            r = dict(reps[0])
            r["repetitions"] = REPS
            r["outcomes_over_reps"] = [x["outcome"] for x in reps]
            r["charges_over_reps"] = [x["charges_in_ledger"] for x in reps]
            r["pre_resume_step_over_reps"] = [x["pre_resume_recorded"]["step"] for x in reps]
            res[w] = r
        with open(os.path.join(work, f"e2_{m}.json"), "w") as f:
            json.dump(res, f, indent=1)
        for w, r in res.items():
            print(m, w, r["killed_at"], r["start_returncode"], r["outcomes_over_reps"], "charges=", r["charges_over_reps"], "steps=", r["pre_resume_step_over_reps"],
                  "lost=", r["progress_lost"], r["redone_tools"], r["redone_model_call_indices"], r["resume_error"])


if __name__ == "__main__":
    main()
