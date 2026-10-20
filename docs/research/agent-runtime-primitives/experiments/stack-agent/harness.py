#!/usr/bin/env python3
"""E1 / E2 / E5 for stack-agent itself, driven through the real release binary (stdlib only).

Mapping of the shared fixture onto stack-agent's fixed capabilities (see README.md):
  lookup  -> project.read src/lib.rs  (an observation; not ledgered: it is Chip's own executor)
  charge  -> pax.test                 (the consequential effect; the fake `pax` ledgers it)
  final   -> Chip's local policy completes `--kind verify` work itself once pax.test passed
The fake model (fake_model.py) answers from what the request shows, not from a counter.

Usage: harness.py BIN_DIR WORK_DIR OUT_DIR [e1|e2|e5|all] [--n N]
  BIN_DIR holds `stack-agent` (release, origin/main 366f6c1) and `sa-state` (sa-state/ here).
"""
import json
import os
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
GOAL = "Establish whether the project passes its tests."
TASK = "e2-task"


class Env:
    def __init__(self, bin_dir, work, out):
        self.bin = os.path.join(bin_dir, "stack-agent")
        self.sa_state = os.path.join(bin_dir, "sa-state")
        # P3 ships `stack-agent agent-state settle`; an earlier binary settles through sa-state.
        help_text = subprocess.run([self.bin, "--help"], capture_output=True, text=True).stdout
        self.has_settle = "agent-state" in help_text
        self.work, self.out = work, out
        os.makedirs(work, exist_ok=True)
        os.makedirs(os.path.join(out, "logs"), exist_ok=True)


class Case:
    def __init__(self, env, name):
        self.env, self.name = env, name
        self.dir = os.path.join(env.work, name)
        shutil.rmtree(self.dir, ignore_errors=True)
        self.proj = os.path.join(self.dir, "proj")
        os.makedirs(os.path.join(self.proj, "src"))
        with open(os.path.join(self.proj, "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "probe"\nversion = "0.1.0"\nedition = "2021"\n')
        with open(os.path.join(self.proj, "src", "lib.rs"), "w") as f:
            f.write("pub fn one() -> u8 { 1 }\n")
        subprocess.run(["git", "init", "-q", "."], cwd=self.proj, check=True)
        self.ledger = os.path.join(self.dir, "ledger.jsonl")
        self.db = os.path.join(self.dir, "state.log")
        self.data = os.path.join(self.dir, "data")
        self.pidfile = os.path.join(self.dir, "agent.pid")
        self.model_log = os.path.join(self.dir, "model.log")
        self.model = None
        self.port = None

    # ---- fake model -------------------------------------------------------------------------
    def start_model(self, kill_on=None, invalid=None):
        self.stop_model()
        port_file = os.path.join(self.dir, "port")
        if os.path.exists(port_file):
            os.remove(port_file)
        cmd = [sys.executable, "-I", os.path.join(HERE, "fake_model.py"), "--port-file", port_file,
               "--log", self.model_log, "--pidfile", self.pidfile]
        if kill_on:
            cmd += ["--kill-on", kill_on]
        if invalid:
            cmd += ["--invalid", invalid]
        self.model = subprocess.Popen(cmd)
        while not os.path.exists(port_file):  # readiness handshake, not a kill timing
            time.sleep(0.01)
        self.port = int(open(port_file).read())

    def stop_model(self):
        if self.model:
            self.model.terminate()
            self.model.wait()
            self.model = None

    # ---- the agent --------------------------------------------------------------------------
    def run(self, durable=True, pax_mode="normal", gdb=None, lldb=None, lease_secs=1, tag="run"):
        env = {k: v for k, v in os.environ.items()
               if k not in ("PAX_BIN", "CHIP_EXECUTOR", "ANTHROPIC_API_KEY", "OPENAI_API_KEY")}
        env.update({
            "PATH": os.path.join(HERE, "fake-pax") + os.pathsep + os.environ["PATH"],
            "CHIP_PROVIDER": "openai-compatible", "CHIP_MODEL": "scripted",
            "CHIP_ENDPOINT": f"http://127.0.0.1:{self.port}/v1/chat/completions",
            "CHIP_API_KEY": "sk-fake-scripted", "STACK_AGENT_DATA_DIR": self.data,
            "FAKE_PAX_LEDGER": self.ledger, "FAKE_PAX_MODE": pax_mode, "FAKE_PAX_INVOCATION": tag,
        })
        argv = [self.env.bin, "work", GOAL, "--kind", "verify", "--json"]
        if durable:
            argv += ["--state-db", self.db, "--task-id", TASK]
            if lease_secs is not None:
                argv += ["--lease-secs", str(lease_secs)]
        gdb_report = os.path.join(self.dir, f"gdb-{tag}.json")
        if gdb:
            env.update({"GDBKILL": gdb, "GDBKILL_DB": self.db, "GDBKILL_REPORT": gdb_report})
            argv = ["gdb", "-q", "-batch", "-x", os.path.join(HERE, "gdb_kill.py"), "--args"] + argv
        # The shell writes its own pid, then execs the agent in place, so the pid the fake model
        # reads is the agent's (or strace's, when wrapped; strace is never used with --kill-on).
        lldb_report = os.path.join(self.dir, f"lldb-{tag}.json")
        if lldb:
            # The macOS counterpart of gdb: same commit boundaries, no code change.
            env.update({"LLDBKILL": lldb, "LLDBKILL_DB": self.db, "LLDBKILL_REPORT": lldb_report,
                        "PYTHONPATH": HERE})
            argv = ["lldb", "-b", "-o", f"command script import {os.path.join(HERE, 'lldb_kill.py')}",
                    "-o", "run", "--"] + argv
        sh = ["/bin/sh", "-c", 'echo $$ > "$0"; exec "$@"', self.pidfile] + argv
        t0 = time.perf_counter()
        p = subprocess.run(sh, cwd=self.proj, env=env, stdin=subprocess.DEVNULL,
                           capture_output=True, text=True)
        wall = (time.perf_counter() - t0) * 1000
        report = None
        for line in reversed(p.stdout.strip().splitlines()):
            try:
                report = json.loads(line)
                break
            except ValueError:
                continue
        killed = p.returncode == -signal.SIGKILL
        gdb_info = None
        if gdb:
            gdb_info = json.load(open(gdb_report))
            killed = gdb_info["fired"] is not None
            report = None  # gdb shares the agent's stdout
        if lldb:
            gdb_info = json.load(open(lldb_report))
            killed = gdb_info["fired"] is not None
            report = None  # lldb shares the agent's stdout
        return {"tag": tag, "exit": p.returncode, "killed_by_sigkill": killed, "gdb": gdb_info,
                "wall_ms": round(wall, 2), "stderr": p.stderr.strip()[-1500:], "report": report}

    def history(self):
        if not os.path.exists(self.db):
            return None
        p = subprocess.run([self.env.sa_state, "history", self.db, TASK], capture_output=True, text=True)
        try:
            return json.loads(p.stdout)
        except ValueError:
            return {"error": p.stderr.strip()}

    def reconcile(self, key, executed):
        p = subprocess.run([self.env.sa_state, "reconcile", self.db, TASK, key,
                            "executed" if executed else "not_executed"], capture_output=True, text=True)
        return {"exit": p.returncode, "stdout": p.stdout.strip(), "stderr": p.stderr.strip()}

    def settle(self, key, executed, evidence):
        """The operator's settlement: the shipped command if the binary has one, else sa-state."""
        t0 = time.perf_counter()
        if self.env.has_settle:
            argv = [self.env.bin, "agent-state", "settle", "--state-db", self.db, "--task-id", TASK,
                    "--intent", key, "--executed", "true" if executed else "false",
                    "--decided-by", "harness-operator", "--evidence", evidence, "--json"]
        else:
            argv = [self.env.sa_state, "reconcile", self.db, TASK, key,
                    "executed" if executed else "not_executed"]
        p = subprocess.run(argv, capture_output=True, text=True)
        return {"how": "stack-agent agent-state settle" if self.env.has_settle else "sa-state reconcile",
                "exit": p.returncode, "wall_ms": round((time.perf_counter() - t0) * 1000, 2),
                "stdout": p.stdout.strip()[-1500:], "stderr": p.stderr.strip()[-500:]}

    def ledger_lines(self):
        if not os.path.exists(self.ledger):
            return []
        return [json.loads(l) for l in open(self.ledger) if l.strip()]

    def db_transactions(self):
        if not os.path.exists(self.db):
            return 0
        return sum(1 for l in open(self.db) if '"feltdb.transaction.v1"' in l)


def summarize_history(h):
    if not h or "summary" not in h:
        return h
    return {
        "status": h["summary"]["status"], "fence": h["summary"]["fence"],
        "checkpoint_version": h["summary"]["checkpoint_version"],
        "intents": [(i["key"], i["action"], i["state"], i.get("result")) for i in h["intents"]],
        "attempts_with_result": [r["attempt"] for r in h["results"]],
        "checkpoints": [(c["version"], c["status"]) for c in h["checkpoints"]],
    }


def brief(run):
    r = run["report"] or {}
    st = r.get("agent_state") or {}
    return {
        "tag": run["tag"], "exit": run["exit"], "killed_by_sigkill": run["killed_by_sigkill"],
        "wall_ms": run["wall_ms"], "gdb": run.get("gdb"), "ran": r.get("ran"), "terminal_state": r.get("terminal_state"),
        "verified": r.get("verified"),
        "executions_by_capability": r.get("executions_by_capability"),
        "model_calls": (r.get("measurement") or {}).get("model_calls"),
        "agent_state": {k: st.get(k) for k in ("attempt", "fence", "resumed_from", "recorded",
                                                 "completion_refused", "reobserved")} if st else None,
        "stderr_tail": run["stderr"][-400:] if run["exit"] not in (0,) else None,
    }


# ---- E1 ---------------------------------------------------------------------------------------
def e1(env):
    out = {}
    c = Case(env, "e1")
    c.start_model()
    run = c.run(durable=True, tag="e1")
    c.stop_model()
    led = c.ledger_lines()
    h = c.history()
    r = run["report"]
    out["main"] = {
        "success": run["exit"] == 0 and len(led) == 1 and h["summary"]["status"] == "completed",
        "exit": run["exit"], "charges_in_ledger": len(led),
        "task_status": h["summary"]["status"], "history": summarize_history(h),
        "model_calls": r["measurement"]["model_calls"],
        "tool_calls": sum(r["executions_by_capability"].values()),
        "executions_by_capability": r["executions_by_capability"],
        "invalid_inputs": r.get("invalid_inputs"), "invalid_decisions": r.get("invalid_decisions"),
        "tokens": r["measurement"].get("model_tokens"), "wall_ms": run["wall_ms"],
        "decisions": h["results"][0]["decisions"],
    }
    for variant in ("read_missing_path", "test_extra_input"):
        c = Case(env, f"e1-invalid-{variant}")
        c.start_model(invalid=variant)
        run = c.run(durable=True, tag=f"e1-{variant}")
        c.stop_model()
        led = c.ledger_lines()
        h = c.history()
        r = run["report"] or {}
        obs = [o for o in (r.get("observations") or [])]
        out[f"invalid_{variant}"] = {
            "exit": run["exit"], "charges_in_ledger": len(led),
            "terminal_state": r.get("terminal_state"), "outcome_reason": r.get("outcome_reason"),
            "task_status": h["summary"]["status"] if h and "summary" in h else None,
            "invalid_inputs": r.get("invalid_inputs"), "invalid_decisions": r.get("invalid_decisions"),
            "model_calls": (r.get("measurement") or {}).get("model_calls"),
            "executions_by_capability": r.get("executions_by_capability"),
            "intents_recorded": [(i[1], i[2], i[3]) for i in summarize_history(h)["intents"]] if h else None,
            "model_log": [json.loads(l) for l in open(c.model_log)],
            "report_keys_mentioning_invalid": {k: r[k] for k in r if "invalid" in k or "reject" in k},
        }
    return out


# ---- E2 ---------------------------------------------------------------------------------------
# Commit order of one uninterrupted durable run (verified: logs/commit_map_raw.json):
#  T1 create_task  T2 checkpoint(running)  T3 renew  T4 record_intent(read)  T5 obs(read)
#  T6 record_outcome(read)  [pax --version probe = validation of the pax.test request]
#  T7 renew  T8 record_intent(pax.test)  [pax test runs]  T9 obs(pax.test)  T10 record_outcome(pax.test)
#  T11 chip.WorkResult  T12 checkpoint(completed)
# Each commit: openat(O_APPEND) -> hundreds of small write()s of one JSON line -> fdatasync().
# Kills at commit boundaries go through gdb_kill.py (ptrace syscall catchpoints, no code change).
WINDOWS = {
    "W1a": {"desc": "model chose pax.test; chip-pax is validating it (its `pax --version` probe; fake pax SIGKILLs its parent), before renew/record_intent", "pax_mode": "kill_on_probe"},
    "W1a-model": {"desc": "while the model is being asked the decision that leads to pax.test (fake model SIGKILLs the agent pid on that request); same persisted state as W1a, decisions are not persisted", "kill_on": "ask_test"},
    "W1b": {"desc": "record_intent(pax.test) committed (T8), inner executor started, before the effect (fake pax SIGKILLs its parent before the ledger write)", "pax_mode": "kill_before"},
    "W2": {"desc": "inside pax.test after the ledger fsync, before it returns (fake pax SIGKILLs its parent)", "pax_mode": "kill_after"},
    "W3": {"desc": "pax.test returned and record_outcome(pax.test) (T10) is durable (fdatasync returned); before chip.WorkResult", "gdb": "commit:10:after"},
    "W4a": {"desc": "chip.Observation(pax.test) (T9) durable, record_outcome (T10) not started", "gdb": "commit:10:before"},
    "W4b": {"desc": "chip.WorkResult (T11) durable, terminal checkpoint (T12) not started", "gdb": "commit:12:before"},
    "W4c": {"desc": "effect done (ledger), at entry of the first write() of chip.Observation(pax.test) (T9): nothing of T9 on disk", "gdb": "commit:9:torn:1"},
    "W4-torn-obs": {"desc": "torn chip.Observation(pax.test) line: SIGKILL at entry of write() 250 of T9 (249 small writes landed)", "gdb": "commit:9:torn:250"},
    "W4-torn-outcome": {"desc": "torn record_outcome(pax.test) line: SIGKILL at entry of write() 380 of T10", "gdb": "commit:10:torn:380"},
    "W4-torn-final": {"desc": "torn terminal checkpoint line: SIGKILL at entry of write() 430 of T12", "gdb": "commit:12:torn:430"},
    "W4d": {"desc": "record_intent(pax.test) (T8) fully written, SIGKILL at entry of its fdatasync", "gdb": "commit:8:unsynced"},
    # P4: a kill inside an observation. record_intent(project.read) (T4) is durable; the read
    # has not yet returned and chip.Observation(project.read) (T5) is not started.
    "W-read": {"desc": "inside project.read: record_intent(project.read) (T4) durable, its observation (T5) not started", "gdb": "commit:4:after"},
    "W-read-unsynced": {"desc": "inside project.read, earlier: record_intent(project.read) (T4) written, SIGKILL at entry of its sync", "gdb": "commit:4:unsynced"},
}
GDB_LEASE = 30  # ptrace stops slow the agent; a 1 s lease would expire between its own commits
# Where gdb is not available (macOS), the commit-boundary and torn-write windows are driven by
# lldb_kill.py.
USE_LLDB = platform.system() == "Darwin"
WINDOW_SET = os.environ.get("HARNESS_WINDOWS")  # comma-separated subset, for a partial run


def classify(charges, final_status, progress_lost, crashed, settled_executed=False):
    if crashed:
        return "crashed_on_resume"
    if charges >= 2:
        return "duplicate_effect"
    # The charge was settled as executed, so it is not performed again; the verify goal needs its
    # (never observed) result, so the work stops rather than complete or repeat it.
    if final_status != "completed" and settled_executed and charges == 1:
        return "stopped_after_settlement"
    if final_status != "completed":
        return "stuck"
    if progress_lost:
        return "lost_progress"
    return "correct" if charges == 1 else "lost_progress"


def resume_and_settle(c, rec):
    """Resume in fresh processes; settle OutcomeUncertain the only documented way (operator
    writes the observation, from the ledger), then resume again."""
    time.sleep(rec["lease_secs"] + 0.5)  # lets the killed owner's lease expire (contract v1 has no forced takeover)
    c.start_model()
    r1 = c.run(durable=True, lease_secs=rec["lease_secs"], tag="resume1")
    rec["resume"] = brief(r1)
    rec["history_after_resume"] = summarize_history(c.history())
    rec["ledger_after_resume"] = len(c.ledger_lines())
    rec["refused_outcome_uncertain"] = r1["exit"] == 3 and "outcome is unknown" in r1["stderr"]
    rec["operator_steps"] = 0
    final = r1
    if rec["refused_outcome_uncertain"]:
        h = c.history()
        open_keys = [i["key"] for i in h["intents"] if i["state"] != "resolved"]
        # Ground truth for the operator: the ledger line written by the killed process.
        executed = rec["ledger_at_kill"] > 0
        t_refused = time.perf_counter()
        results = [c.settle(k, executed, f"harness ledger has {rec['ledger_at_kill']} line(s) at the kill")
                   for k in open_keys]
        rec["manual_intervention"] = {
            "what": ("operator settled each open intent with the shipped `stack-agent agent-state settle`, "
                     "conclusion from the ledger" if c.env.has_settle else
                     "operator wrote chip.Observation for each open intent via sa-state reconcile (no product command; same record durability_bench.rs writes)"),
            "open_intents": open_keys, "conclusion_from_ledger": "executed" if executed else "not_executed",
            "results": results,
        }
        rec["operator_steps"] = len(open_keys)
        rec["fence_after_refused_resume"] = (c.history() or {}).get("summary", {}).get("fence")
        if not c.env.has_settle:
            # Before P3 the refused resume1 kept the lease it had just taken, so it must expire.
            time.sleep(rec["lease_secs"] + 0.5)
        rec["reacquire_after_refusal_ms"] = round((time.perf_counter() - t_refused) * 1000, 2)
        r2 = c.run(durable=True, lease_secs=rec["lease_secs"], tag="resume2")
        rec["resume_after_manual"] = brief(r2)
        rec["history_after_manual"] = summarize_history(c.history())
        final = r2
    c.stop_model()
    h = c.history()
    led = c.ledger_lines()
    final_status = h["summary"]["status"] if h and "summary" in h else None
    rep = final["report"] or {}
    reads_redone = (rep.get("executions_by_capability") or {}).get("project.read", 0) > 0 and rec["read_recorded_before_kill"]
    rec["charges_in_ledger"] = len(led)
    rec["ledger_invocations"] = [l["invocation"] for l in led]
    rec["final_task_status"] = final_status
    rec["resumed"] = final["exit"] == 0 and not rec["refused_outcome_uncertain"]
    rec["progress_lost"] = bool(reads_redone)
    rec["final_state_consistent"] = (final_status == "completed" and len(led) >= 1)
    crashed = final["exit"] not in (0, 1, 3) and not final["killed_by_sigkill"]
    if rec["refused_outcome_uncertain"]:
        rec["outcome"] = "stuck"
        settled_executed = rec["manual_intervention"]["conclusion_from_ledger"] == "executed"
        rec["outcome_after_manual_resolution"] = classify(len(led), final_status, reads_redone, crashed,
                                                          settled_executed)
        rec["final_terminal_state"] = rep.get("terminal_state")
        rec["final_reason"] = rep.get("outcome_reason")
    else:
        rec["outcome"] = classify(len(led), final_status, reads_redone, crashed)
    return rec


def e2_window(env, name, spec):
    c = Case(env, f"e2-{name}")
    c.start_model(kill_on=spec.get("kill_on"))
    lease = GDB_LEASE if "gdb" in spec else 1
    debugger = {"lldb": spec.get("gdb")} if USE_LLDB else {"gdb": spec.get("gdb")}
    crash = c.run(durable=True, pax_mode=spec.get("pax_mode", "normal"), **debugger,
                  lease_secs=lease, tag="crash")
    c.stop_model()
    # Measured on the raw file before anything reopens it (FeltDB truncates a torn tail on open).
    raw = open(c.db, "rb").read() if os.path.exists(c.db) else b""
    tail = raw[raw.rfind(b"\n") + 1:]
    rec = {"kill_point": spec["desc"], "lease_secs": lease, "crash_run": brief(crash),
           "kill_reached": crash["killed_by_sigkill"],
           "db_bytes_at_kill": len(raw), "db_torn_tail_bytes": len(tail),
           "db_transactions_at_kill": raw.count(b'"record_type":"feltdb.transaction.v1"') - (1 if tail.startswith(b'{"record_type":"feltdb.transaction.v1"') else 0),
           "ledger_at_kill": len(c.ledger_lines())}
    h = c.history()
    sh = summarize_history(h)
    rec["history_at_kill"] = sh
    rec["db_bytes_after_reopen"] = os.path.getsize(c.db) if os.path.exists(c.db) else None
    rec["read_recorded_before_kill"] = bool(sh and any(i[1] == "project.read" and i[2] == "resolved" for i in sh["intents"]))
    return resume_and_settle(c, rec)


def e2(env):
    windows = {}
    for name, spec in WINDOWS.items():
        if WINDOW_SET and name not in WINDOW_SET.split(","):
            continue
        windows[name] = e2_window(env, name, spec)
        print(name, windows[name]["outcome"], windows[name].get("outcome_after_manual_resolution"),
              windows[name].get("charges_in_ledger"), file=sys.stderr)
    if WINDOW_SET:
        return windows
    # Restart with the default lease (900 s): the killed owner's lease blocks every restart.
    c = Case(env, "e2-W2-default-lease")
    c.start_model()
    crash = c.run(durable=True, pax_mode="kill_after", lease_secs=None, tag="crash")
    again = c.run(durable=True, lease_secs=None, tag="resume-immediately")
    c.stop_model()
    windows["W2-default-lease"] = {
        "kill_point": "as W2, default --lease-secs (900 s); resumed immediately",
        "crash_run": brief(crash), "resume": brief(again), "charges_in_ledger": len(c.ledger_lines()),
        "history": summarize_history(c.history()), "resumed": again["exit"] == 0,
        "outcome": "stuck" if again["exit"] == 3 else None,
        "manual_intervention": "wait until the dead owner's lease expires (up to 900 s); contract v1 has no forced takeover",
    }
    # The refusing resume runs with the default lease (900 s). Before P3 it kept that lease, so the
    # settled task could not be resumed for 15 minutes; with P3 the refusal gives it back.
    c = Case(env, "e2-W2-refusal-default-lease")
    c.start_model()
    crash = c.run(durable=True, pax_mode="kill_after", lease_secs=1, tag="crash")
    time.sleep(1.5)
    r1 = c.run(durable=True, lease_secs=None, tag="resume1")
    open_keys = [i["key"] for i in (c.history() or {}).get("intents", []) if i["state"] != "resolved"]
    t0 = time.perf_counter()
    settled = [c.settle(k, True, "harness ledger has 1 line at the kill") for k in open_keys]
    r2 = c.run(durable=True, lease_secs=None, tag="resume2")
    c.stop_model()
    windows["W2-refusal-default-lease"] = {
        "kill_point": "as W2 (crash lease 1 s, waited out); the refused resume and the next one use the default 900 s lease",
        "crash_run": brief(crash), "resume1": brief(r1), "settle": settled, "resume2": brief(r2),
        "resume2_after_ms": round((time.perf_counter() - t0) * 1000, 2),
        "resume2_lease_held": r2["exit"] == 3 and "unexpired lease" in r2["stderr"],
        "charges_in_ledger": len(c.ledger_lines()), "history": summarize_history(c.history()),
    }
    windows["W5"] = {"outcome": "not_applicable", "notes": "stack-agent has no handoff/transfer primitive (baseline §4)"}
    windows["W6"] = {"outcome": "not_applicable", "notes": "stack-agent has no approval/suspension state; Escalate is terminal (baseline §4)"}
    return windows


def commit_map(env):
    """One uninterrupted durable run; which record each db line (transaction) carries."""
    import re
    c = Case(env, "commit-map")
    c.start_model()
    run = c.run(durable=True, tag="map")
    c.stop_model()
    rows = []
    for line in open(c.db):
        if '"feltdb.transaction.v1"' not in line:
            continue
        caps = sorted(set(re.findall(r'"capability":"([\w.]+)"', line)))
        act = re.findall(r'"action":"([\w.]+)"', line)
        st = re.findall(r'"status":"(\w+)"', line)
        state = re.findall(r'"state":"(\w+)"', line)
        rows.append({"n": len(rows) + 1, "collections": caps, "action": act[:1], "status": st[:2], "intent_state": state[:1], "bytes": len(line)})
    return {"exit": run["exit"], "transactions": rows}


# ---- E5 ---------------------------------------------------------------------------------------
def pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    f = int(k)
    return round(xs[f] + (xs[min(f + 1, len(xs) - 1)] - xs[f]) * (k - f), 2)


def e5(env, n):
    res = {"repetitions": n, "warmup": 1}
    for label, durable in (("in_memory_ms", False), ("durable_ms", True)):
        times, ok = [], 0
        for i in range(n + 1):
            c = Case(env, f"e5-{label}-{i}")
            c.start_model()
            run = c.run(durable=durable, tag="e5")
            c.stop_model()
            good = run["exit"] == 0 and len(c.ledger_lines()) == 1
            if i > 0:
                times.append(run["wall_ms"])
                ok += good
            shutil.rmtree(c.dir, ignore_errors=True)
        res[label] = {"p10": pct(times, 0.1), "median": round(statistics.median(times), 2),
                      "p90": pct(times, 0.9), "min": min(times), "max": max(times), "verified_runs": ok}
    # cold start: a usage error exits before any provider, model or project work
    times = []
    for _ in range(21):
        t0 = time.perf_counter()
        subprocess.run([env.bin, "work"], capture_output=True)
        times.append((time.perf_counter() - t0) * 1000)
    times = times[1:]
    res["cold_start_ms"] = {"what": "`stack-agent work` with no goal (usage error, exit 2): process start + argument parsing",
                            "p10": pct(times, 0.1), "median": round(statistics.median(times), 2), "p90": pct(times, 0.9)}
    return res


def main():
    bin_dir, work, out = sys.argv[1:4]
    what = sys.argv[4] if len(sys.argv) > 4 else "all"
    n = int(sys.argv[sys.argv.index("--n") + 1]) if "--n" in sys.argv else 50
    env = Env(bin_dir, work, out)
    raw = {}
    if what in ("map", "all", "e2"):
        raw["commit_map"] = commit_map(env)
    if what in ("e1", "all"):
        raw["e1"] = e1(env)
    if what in ("e2", "all"):
        raw["e2"] = e2(env)
    if what in ("e5", "all"):
        raw["e5"] = e5(env, n)
    for k, v in raw.items():
        with open(os.path.join(out, "logs", f"{k}_raw.json"), "w") as f:
            json.dump(v, f, indent=1)
    print(json.dumps({k: (list(v) if isinstance(v, dict) else v) for k, v in raw.items()}))


if __name__ == "__main__":
    main()
