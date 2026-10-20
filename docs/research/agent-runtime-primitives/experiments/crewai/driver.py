"""Runs E1 and E2 windows for the CrewAI Flow harness and writes results.json.

  python -I driver.py <venv-python> <scratch-root>
"""
import json, os, platform, shutil, subprocess, sys

PY, ROOT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
H = os.path.join(HERE, "harness.py")
ENV_EXTRA = {"CREWAI_DISABLE_TELEMETRY": "true", "OTEL_SDK_DISABLED": "true", "CREWAI_TRACING_ENABLED": "false"}


def ledger(w):
    p = os.path.join(w, "ledger.jsonl")
    return [json.loads(l) for l in open(p) if l.strip()] if os.path.exists(p) else []


def run(mode, w, window, mech):
    env = {**os.environ, **ENV_EXTRA, "HOME": w}
    p = subprocess.run([PY, "-I", H, mode, w, window, mech], cwd=w, env=env, capture_output=True, text=True, timeout=180)
    return p.returncode, p.stdout, p.stderr[-1500:]


def rows_at_kill(w):
    import sqlite3
    out = {}
    for db, q in (("flow.db", "SELECT method_name FROM flow_states ORDER BY id"), ("ckpt.db", "SELECT json_extract(data,'$.trigger') FROM checkpoints ORDER BY rowid")):
        p = os.path.join(w, "store", db)
        if os.path.exists(p):
            try:
                with sqlite3.connect(p) as c:
                    out[db] = [r[0] for r in c.execute(q)]
            except sqlite3.OperationalError as e:
                out[db] = f"error: {e}"
    return out


def window(name, mech, rep=0):
    w = os.path.join(ROOT, f"{mech}-{name}-{rep}")
    shutil.rmtree(w, ignore_errors=True); os.makedirs(w)
    rc1, out1, _ = run("run", w, name, mech)
    before = ledger(w)
    store_at_kill = rows_at_kill(w)
    rc2, out2, err2 = run("resume", w, name, mech)
    after = ledger(w)
    rp = os.path.join(w, "resume.json")
    res = json.load(open(rp)) if os.path.exists(rp) else {}
    charges = sum(l["tool"] == "charge" for l in after)
    lookups = sum(l["tool"] == "lookup" for l in after)
    finished = res.get("output") == "Charged acct-7 42"
    lost = lookups > 1
    if rc2 != 0 or not res:
        outcome = "crashed_on_resume"
    elif res.get("error"):
        outcome = "stuck"
    elif charges > 1:
        outcome = "duplicate_effect"
    elif not finished:
        outcome = "stuck"
    elif lost:
        outcome = "lost_progress"
    else:
        outcome = "correct"
    sc = res.get("state_charge_results")
    return {
        "killed_with_sigkill": rc1 == -9, "first_rc": rc1,
        "resumed": rc2 == 0 and bool(res) and (finished or res.get("pending")),
        "resumed_via": res.get("resumed_via"), "resumed_from": res.get("resumed_from"),
        "from_pending_error": res.get("from_pending_error"),
        "charges_before_kill": sum(l["tool"] == "charge" for l in before),
        "charges_in_ledger": charges, "lookups_in_ledger": lookups,
        "final_state_consistent": (sc == charges) if sc is not None else None,
        "progress_lost": lost,
        "model_calls_resume_process": res.get("model_calls_this_process"),
        "manual_intervention": "none beyond the documented resume call (flow id kept by the harness)",
        "store_rows_at_kill": store_at_kill, "resume_error": res.get("error"),
        "outcome": outcome,
        "stderr_resume_tail": err2[-600:] if rc2 != 0 else "",
    }


w = os.path.join(ROOT, "E1"); shutil.rmtree(w, ignore_errors=True); os.makedirs(w)
rc, out, err = run("run", w, "none", "persist")
r = json.loads(out.strip().splitlines()[-1]); L = ledger(w)
e1 = {"success": rc == 0 and [l["tool"] for l in L] == ["lookup", "charge"] and L[-1]["args"]["amount"] == 42,
      "model_calls": r["model_calls_this_process"], "tool_calls": len(L), "invalid_args_handled": None,
      "retries": 0, "tokens": None, "wall_ms": r["wall_ms"],
      "notes": "Fake model and tools are Flow methods (no LLM, no Agent). wall_ms includes CrewAI console/event-bus overhead; not an E5 measurement."}

windows = {f"persist/{k}": window(k, "persist") for k in ("W1", "W2", "W3", "W4")}
for k in ("W1", "W2", "W3"):
    for rep in range(3):  # checkpoint writes are dispatched on a thread pool: repeat to show variance
        windows[f"ckpt/{k}#{rep}"] = window(k, "ckpt", rep)
windows["approval/W6"] = window("W6", "approval")
windows["W5"] = {"outcome": "not_applicable", "notes": "Delegation needs an LLM agent; Flows have no handoff."}

import importlib.metadata as md  # noqa: E402  (driver runs under system python; version read from venv below)
ver = subprocess.run([PY, "-I", "-c", "import importlib.metadata as m;print(m.version('crewai'))"], capture_output=True, text=True).stdout.strip()
result = {
    "candidate": "crewai", "commit": "c6e500ab3730ccf3badb640c6e0170e5c6aa3882", "package": f"crewai=={ver}",
    "environment": {"cpus": os.cpu_count(), "python": platform.python_version(), "os": platform.system().lower(), "machine": platform.machine()},
    "e1": e1,
    "e2": {"store": "persist: SQLiteFlowPersistence (WAL); ckpt: CheckpointConfig(SqliteProvider, on_events=[method_execution_finished])",
           "windows": windows},
    "e3": None, "e5": None,
    "blocked": ["E4: no local inference endpoint", "W5: no handoff in Flows; Crew delegation needs an LLM"],
}
json.dump(result, open(os.path.join(HERE, "results.json"), "w"), indent=2)
print(json.dumps({k: (v.get("outcome"), v.get("charges_in_ledger"), v.get("lookups_in_ledger")) for k, v in windows.items()}, indent=0))
