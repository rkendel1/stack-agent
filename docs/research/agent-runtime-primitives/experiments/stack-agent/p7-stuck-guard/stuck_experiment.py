#!/usr/bin/env python3
"""Stuck-guard experiment: identical scripted-model scenarios against two stack-agent binaries.

Usage: stuck_experiment.py BASELINE_BIN CANDIDATE_BIN WORK_DIR OUT_JSON
Each BIN is a `stack-agent` release binary. For every scenario and binary it starts a fresh scripted
model (scenario_model.py), a fresh project and the fake `pax` of the E1/E2 harness, runs
`stack-agent work --json`, and records what the report and the model's log say. Nothing here
measures a real model: the "model" asks for exactly what the scenario scripts.
"""
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
FAKE_PAX = os.path.join(HERE, "..", "fake-pax")
GOAL = "Look into `one`."


def d(capability, **inputs):
    o = {"decision": "request_capability", "capability": capability}
    if inputs:
        o["inputs"] = inputs
    return json.dumps(o)


LIB1 = "pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"
LIB2 = LIB1 + "pub fn three() -> u8 { 3 }\n"
LIB3 = LIB2 + "pub fn four() -> u8 { 4 }\n"
READ = d("project.read", path="src/lib.rs")
LIST = d("project.list", path=".")
LIST_SRC = d("project.list", path="src")
SEARCH = d("project.search", query="one")
CARGO = d("project.read", path="Cargo.toml")
W1, W2, W3 = (d("project.write", path="src/lib.rs", content=c) for c in (LIB1, LIB2, LIB3))
TEST = d("pax.test")
BIG = ["--max-turns", "20", "--max-executions", "20"]

SCENARIOS = [
    # name, what it is, script (the last reply repeats), extra args, intended outcome
    ("identical-loop", "the same read, forever", [READ], BIG, "stops: stuck"),
    ("alternating-loop", "read and list, alternating forever", [READ, LIST] * 10, BIG, "stops: stuck"),
    ("productive-repeats", "write, read, write, read, write, read, test: legitimate repeated capability calls",
     [W1, READ, W2, READ, W3, READ, TEST], BIG, "completes"),
    ("ordinary-task", "write, test", [W1, TEST], [], "completes"),
    ("turn-limit-no-loop", "six different requests with --max-turns 4",
     [READ, LIST, SEARCH, LIST_SRC, CARGO, READ], ["--max-turns", "4", "--max-executions", "20"], "turn limit"),
    ("loop-after-progress", "write, read, then the same read forever", [W1, READ], BIG, "stops: stuck, progress kept"),
    ("identical-loop-default-limits", "the same read forever, at the default limits (8 turns, 4 executions)",
     [READ], [], "stops: stuck"),
]


def run_once(bin_path, case_dir, script, extra, tag, state=None, port_state=None):
    env = {k: v for k, v in os.environ.items()
           if k not in ("PAX_BIN", "CHIP_EXECUTOR", "ANTHROPIC_API_KEY", "OPENAI_API_KEY")}
    env.update({
        "PATH": os.path.abspath(FAKE_PAX) + os.pathsep + os.environ["PATH"],
        "CHIP_PROVIDER": "openai-compatible", "CHIP_MODEL": "scripted",
        "CHIP_ENDPOINT": f"http://127.0.0.1:{port_state['port']}/v1/chat/completions",
        "CHIP_API_KEY": "sk-fake-scripted", "STACK_AGENT_DATA_DIR": os.path.join(case_dir, "data"),
        "FAKE_PAX_LEDGER": os.path.join(case_dir, "ledger.jsonl"), "FAKE_PAX_MODE": "normal",
        "FAKE_PAX_INVOCATION": tag,
    })
    argv = [bin_path, "work", GOAL, "--kind", "change", "--json"] + extra
    if state:
        argv += state
    t0 = time.perf_counter()
    p = subprocess.run(argv, cwd=os.path.join(case_dir, "proj"), env=env, capture_output=True,
                       text=True, stdin=subprocess.DEVNULL)
    wall = (time.perf_counter() - t0) * 1000
    report = None
    for line in reversed(p.stdout.strip().splitlines()):
        try:
            report = json.loads(line)
            break
        except ValueError:
            continue
    return p, report, wall


def start_model(case_dir, script):
    sp = os.path.join(case_dir, "script.json")
    json.dump(script, open(sp, "w"))
    port_file = os.path.join(case_dir, "port")
    log = os.path.join(case_dir, "model.log")
    proc = subprocess.Popen([sys.executable, "-I", os.path.join(HERE, "scenario_model.py"),
                             "--port-file", port_file, "--log", log, "--script", sp])
    while not os.path.exists(port_file):
        time.sleep(0.01)
    return proc, {"port": int(open(port_file).read())}, log


def model_log(log):
    if not os.path.exists(log):
        return []
    return [json.loads(l) for l in open(log) if l.strip()]


def project(case_dir):
    proj = os.path.join(case_dir, "proj")
    os.makedirs(os.path.join(proj, "src"))
    open(os.path.join(proj, "Cargo.toml"), "w").write('[package]\nname = "probe"\nversion = "0.1.0"\nedition = "2021"\n')
    open(os.path.join(proj, "src", "lib.rs"), "w").write("pub fn one() -> u8 { 1 }\n")
    subprocess.run(["git", "init", "-q", "."], cwd=proj, check=True)
    return proj


def summarize(p, report, wall, served, ledger):
    r = report or {}
    by = r.get("executions_by_capability") or {}
    replies = [s["reply"] for s in served]
    seen, repeats = set(), 0
    for x in replies:
        repeats += x in seen
        seen.add(x)
    m = r.get("measurement") or {}
    return {
        "exit": p.returncode, "terminal_state": r.get("terminal_state"),
        "stuck": str(r.get("outcome_reason") or "").startswith("stuck: "),
        "outcome_reason": (r.get("outcome_reason") or "")[:160],
        "verified": r.get("verified"),
        "model_calls": m.get("model_calls"), "model_requests_served": len(served),
        "executions": sum(by.values()), "executions_by_capability": by,
        "repeated_requests": repeats, "turns": m.get("turns"), "observations": m.get("observations"),
        "pax_ledger_lines": ledger, "wall_ms": round(wall, 1),
        "stderr_tail": p.stderr.strip()[-200:] if p.returncode not in (0, 1) else None,
    }


def scenario(bin_path, work, label, name, script, extra):
    case_dir = os.path.join(work, label, name)
    shutil.rmtree(case_dir, ignore_errors=True)
    os.makedirs(case_dir)
    proj = project(case_dir)
    proc, port, log = start_model(case_dir, script)
    try:
        p, report, wall = run_once(bin_path, case_dir, script, extra, name, port_state=port)
        ledger = len(open(os.path.join(case_dir, "ledger.jsonl")).readlines()) if os.path.exists(os.path.join(case_dir, "ledger.jsonl")) else 0
        out = summarize(p, report, wall, model_log(log), ledger)
        out["file_changed"] = open(os.path.join(proj, "src", "lib.rs")).read() != "pub fn one() -> u8 { 1 }\n"
        return out
    finally:
        proc.terminate()
        proc.wait()


def restart(bin_path, work, label):
    """The identical loop with a durable task; a fresh process resumes it after the lease expires."""
    name = "restart-consistency"
    case_dir = os.path.join(work, label, name)
    shutil.rmtree(case_dir, ignore_errors=True)
    os.makedirs(case_dir)
    project(case_dir)
    proc, port, log = start_model(case_dir, [READ])
    state = ["--state-db", os.path.join(case_dir, "state.log"), "--task-id", "stuck-task", "--lease-secs", "1"]
    try:
        runs = []
        for i in (1, 2):
            before = len(model_log(log))
            p, report, wall = run_once(bin_path, case_dir, [READ], BIG, f"restart{i}", state=state, port_state=port)
            served = model_log(log)[before:]
            s = summarize(p, report, wall, served, 0)
            s["agent_state_status"] = ((report or {}).get("agent_state") or {}).get("task", {}).get("status") \
                or ((report or {}).get("agent_state") or {}).get("status")
            runs.append(s)
            time.sleep(1.4)
        return {"first": runs[0], "second": runs[1],
                "same_terminal_state": runs[0]["terminal_state"] == runs[1]["terminal_state"],
                "same_reason": runs[0]["outcome_reason"] == runs[1]["outcome_reason"],
                "second_model_calls": runs[1]["model_requests_served"], "second_executions": runs[1]["executions"]}
    finally:
        proc.terminate()
        proc.wait()


def main():
    base, cand, work, out = sys.argv[1:5]
    result = {"scenarios": {n: {"what": w, "intended": i} for n, w, _, _, i in SCENARIOS}}
    for label, bin_path in (("baseline", base), ("candidate", cand)):
        for name, _, script, extra, _ in SCENARIOS:
            result["scenarios"][name][label] = scenario(bin_path, work, label, name, script, extra)
            r = result["scenarios"][name][label]
            print(label, name, r["terminal_state"], "stuck" if r["stuck"] else "", "calls", r["model_requests_served"],
                  "exec", r["executions"], file=sys.stderr)
        result.setdefault("restart", {})[label] = restart(bin_path, work, label)
    json.dump(result, open(out, "w"), indent=1)


if __name__ == "__main__":
    main()
