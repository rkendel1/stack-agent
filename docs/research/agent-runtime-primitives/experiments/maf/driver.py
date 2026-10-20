"""Runs E1 and E2 windows for the MAF harness, each window in a fresh workdir, and writes results.json.

  python -I driver.py <venv-python> <scratch-root>
"""
import json, os, platform, shutil, subprocess, sys, importlib.metadata as md

PY, ROOT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
H = os.path.join(HERE, "harness.py")


def ledger(w):
    p = os.path.join(w, "ledger.jsonl")
    if not os.path.exists(p):
        return []
    return [json.loads(l) for l in open(p) if l.strip()]


def run(mode, w, window, approval):
    args = [PY, "-I", H, mode, w, window] + (["approval"] if approval else [])
    p = subprocess.run(args, cwd=w, capture_output=True, text=True, timeout=120)
    return p.returncode, p.stdout, p.stderr[-2000:]


def window(name, approval=False):
    w = os.path.join(ROOT, f"{name}{'-approval' if approval else ''}")
    shutil.rmtree(w, ignore_errors=True); os.makedirs(w)
    rc1, _, err1 = run("run", w, name, approval)
    killed = rc1 == -9
    before = ledger(w)
    rc2, out2, err2 = run("resume", w, name, approval)
    after = ledger(w)
    res = json.load(open(os.path.join(w, "resume.json"))) if os.path.exists(os.path.join(w, "resume.json")) else {}
    charges = [l for l in after if l["tool"] == "charge"]
    lookups = [l for l in after if l["tool"] == "lookup"]
    finished = res.get("output") == "Charged acct-7 42"
    ckpt_charges = res.get("final_checkpoint_charge_results")
    consistent = ckpt_charges == len(charges) if ckpt_charges is not None else None
    lost = len(lookups) > 1  # lookup recorded before the kill was redone
    if rc2 != 0:
        outcome = "crashed_on_resume"
    elif len(charges) > 1:
        outcome = "duplicate_effect"
    elif not finished:
        outcome = "stuck"
    elif lost:
        outcome = "lost_progress"
    else:
        outcome = "correct"
    return {
        "killed_with_sigkill": killed, "first_rc": rc1,
        "resumed": rc2 == 0 and bool(res.get("resumed_from")),
        "resumed_from": res.get("resumed_from"),
        "charges_in_ledger": len(charges), "lookups_in_ledger": len(lookups),
        "charges_before_kill": sum(1 for l in before if l["tool"] == "charge"),
        "final_state_consistent": consistent, "progress_lost": lost,
        "manual_intervention": "none (resume process loads get_latest() and calls run(checkpoint_id=...[, responses=...]))",
        "model_calls_resume_process": res.get("model_calls_this_process"),
        "outcome": outcome, "stderr_first": err1[-400:] if not killed else "", "stderr_resume": err2[-400:] if rc2 else "",
    }


# E1 baseline
w = os.path.join(ROOT, "E1"); shutil.rmtree(w, ignore_errors=True); os.makedirs(w)
rc, out, err = run("run", w, "none", False)
e1r = json.loads(out.strip().splitlines()[-1])
L = ledger(w)
e1 = {"success": rc == 0 and sum(l["tool"] == "charge" for l in L) == 1 and sum(l["tool"] == "lookup" for l in L) == 1
      and L[-1]["args"]["amount"] == 42, "model_calls": e1r["model_calls_this_process"], "tool_calls": len(L),
      "invalid_args_handled": None, "retries": 0, "tokens": None, "wall_ms": e1r["wall_ms"],
      "notes": "Fake model and tools are workflow executors; MAF has no model in the loop here. invalid-args run not performed (no MAF tool-schema validation involved when tools are executors)."}

windows = {k: window(k) for k in ("W1", "W2", "W3", "W4")}
windows["W6"] = window("W6", approval=True)
windows["W5"] = {"outcome": "not_applicable", "notes": "Handoff orchestration needs a chat-client agent; not exercised (source-level only)."}

result = {
    "candidate": "microsoft-agent-framework",
    "commit": "809cc04f2d610bb48fa4274b6e7f0e96773d576d",
    "package": f"agent-framework-core=={md.version('agent-framework-core')}",
    "environment": {"cpus": os.cpu_count(), "python": platform.python_version(), "os": platform.system().lower(), "machine": platform.machine()},
    "e1": e1,
    "e2": {"store": "FileCheckpointStorage (JSON file per checkpoint, tmp + os.replace, no fsync)", "windows": windows},
    "e3": None, "e5": None,
    "blocked": ["E4: no local inference endpoint", "E3/W5: handoff requires a chat-client agent; not run"],
}
json.dump(result, open(os.path.join(HERE, "results.json"), "w"), indent=2)
print(json.dumps({k: (v.get("outcome"), v.get("charges_in_ledger")) for k, v in windows.items()}))
