"""E3 orchestrator (handoff + approval across process restarts) and E2 windows W5/W6.

usage: python -I e3.py <workdir>   -> writes <workdir>/e3.json
"""
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import read_jsonl  # noqa: E402


def scenario(work, name, steps):
    d = os.path.join(work, "e3", name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    base = dict(os.environ, LEDGER=f"{d}/ledger.jsonl", CALLS=f"{d}/calls.jsonl",
                EVENTS=f"{d}/events.jsonl", NODES=f"{d}/nodes.jsonl")
    for k in ("KILL_AT", "FAIL_SPECIALIST", "RETRY"):
        base.pop(k, None)
    procs = []
    for mode, extra in steps:
        p = subprocess.run([sys.executable, "-I", os.path.join(HERE, "e3_child.py"), f"{d}/state.sqlite", "t-e3", mode],
                           env=dict(base, **extra), capture_output=True, text=True, timeout=120)
        out = next((json.loads(l) for l in p.stdout.splitlines() if l.startswith("{")), None)
        procs.append({"mode": mode, "env": extra, "returncode": p.returncode, "out": out,
                      "stderr_tail": p.stderr[-400:] if p.returncode not in (0, -9) else ""})
    ledger = read_jsonl(base["LEDGER"])
    res = {
        "processes": procs,
        "killed_at": [e["killed_at"] for e in read_jsonl(base["EVENTS"])],
        "charges_in_ledger": sum(l["tool"] == "charge" for l in ledger),
        "lookups_in_ledger": sum(l["tool"] == "lookup" for l in ledger),
        "ledger": ledger,
        "node_entries": read_jsonl(base["NODES"]),
        "model_calls": read_jsonl(base["CALLS"]),
    }
    last = procs[-1]["out"] or {}
    res["finished"] = bool(last.get("after", {}).get("next") == [] and "42" in str(last.get("last_message")))
    with open(f"{d}/result.json", "w") as f:
        json.dump(res, f, indent=1, default=str)
    return res


def main():
    work = sys.argv[1]
    out = {
        # main E3 flow + W6: suspend at approval, SIGKILL while suspended, approve in a fresh process,
        # then send the same approval again after completion (double-approval check)
        "W6_approval_across_restart": scenario(work, "w6", [
            ("start", {"KILL_AT": "W6"}), ("inspect", {}), ("approve", {}), ("approve", {})]),
        # W5: SIGKILL right after triage's handoff write is committed, before specialist runs
        "W5_kill_during_handoff": scenario(work, "w5", [
            ("start", {"KILL_AT": "W5"}), ("inspect", {}), ("resume", {}), ("approve", {})]),
        # specialist raises (no retry policy): error recorded, fresh process resumes with invoke(None)
        "specialist_fails_no_retry": scenario(work, "fail", [
            ("start", {"FAIL_SPECIALIST": "1"}), ("inspect", {}), ("resume", {}), ("approve", {})]),
        # specialist raises once with RetryPolicy(max_attempts=2): retried in-process
        "specialist_fails_with_retry": scenario(work, "retry", [
            ("start", {"FAIL_SPECIALIST": "1", "RETRY": "1"}), ("approve", {})]),
        # resume with Command(resume=...) while the thread is suspended, but with no approval key
        # semantics are app-level; rejection path not exercised
    }
    with open(os.path.join(work, "e3.json"), "w") as f:
        json.dump(out, f, indent=1, default=str)
    for k, r in out.items():
        print(k, "killed", r["killed_at"], "charges", r["charges_in_ledger"], "lookups", r["lookups_in_ledger"],
              "finished", r["finished"], [(p["mode"], p["returncode"], (p["out"] or {}).get("error"),
                                           ((p["out"] or {}).get("after") or (p["out"] or {}).get("before") or {}).get("next"))
                                          for p in r["processes"]])


if __name__ == "__main__":
    main()
