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
    def run(self, durable=True, pax_mode="normal", wrap=None, lease_secs=1, tag="run"):
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
        argv = (wrap or []) + argv
        # The shell writes its own pid, then execs the agent in place, so the pid the fake model
        # reads is the agent's (or strace's, when wrapped; strace is never used with --kill-on).
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
        return {"tag": tag, "exit": p.returncode, "killed_by_sigkill": p.returncode == -signal.SIGKILL,
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
        "wall_ms": run["wall_ms"], "ran": r.get("ran"), "terminal_state": r.get("terminal_state"),
        "verified": r.get("verified"),
        "executions_by_capability": r.get("executions_by_capability"),
        "model_calls": (r.get("measurement") or {}).get("model_calls"),
        "agent_state": {k: st.get(k) for k in ("attempt", "fence", "resumed_from", "recorded",
                                                 "completion_refused")} if st else None,
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
            "task_status": h["summary"]["status"] if h and "summary" in h else None,
            "invalid_inputs": r.get("invalid_inputs"), "invalid_decisions": r.get("invalid_decisions"),
            "rejections": (r.get("compute") or {}).get("rejections") if isinstance(r.get("compute"), dict) else None,
            "model_calls": (r.get("measurement") or {}).get("model_calls"),
            "executions_by_capability": r.get("executions_by_capability"),
            "intents_recorded": [(i[1], i[2], i[3]) for i in summarize_history(h)["intents"]] if h else None,
            "model_log": [json.loads(l) for l in open(c.model_log)],
            "report_keys_mentioning_invalid": {k: r[k] for k in r if "invalid" in k or "reject" in k},
        }
    return out


# ---- E2 ---------------------------------------------------------------------------------------
STRACE = "strace"


def strace_kill(db, syscall, n):
    """SIGKILL the agent at entry of its n-th `syscall` on the state-db file (path-filtered).
    The kernel never runs that syscall. All FeltDB commits run on the agent's main thread
    (verified with strace), so the count is deterministic."""
    return [STRACE, "-f", "-qq", "-o", "/dev/null", "-P", db, "-e", f"trace={syscall}",
            "-e", f"inject={syscall}:signal=KILL:when={n}"]


# Commit order of one uninterrupted durable run (verified: logs/e2_commit_map.json):
#  T1 create_task  T2 checkpoint(running)  T3 renew  T4 record_intent(read)  T5 obs(read)
#  T6 record_outcome(read)  [pax --version probe = validation of the pax.test request]
#  T7 renew  T8 record_intent(pax.test)  [pax test runs]  T9 obs(pax.test)  T10 record_outcome(pax.test)
#  T11 chip.WorkResult  T12 checkpoint(completed)
# Each commit: openat(O_APPEND) -> one write() of one JSON line -> fdatasync().
WINDOWS = {
    "W1a": {"desc": "model chose pax.test (decision received and being validated: the `pax --version` probe), before record_intent", "pax_mode": "kill_on_probe"},
    "W1a-model": {"desc": "while the model is being asked the decision that leads to pax.test (fake model SIGKILLs the agent pid on that request); persisted state is the same as W1a because decisions are not persisted", "kill_on": "ask_test"},
    "W1b": {"desc": "after record_intent(pax.test) committed, inside the inner executor before the effect (fake pax kills its parent before the ledger write)", "pax_mode": "kill_before"},
    "W2": {"desc": "inside pax.test after the ledger fsync, before it returns (fake pax SIGKILLs its parent)", "pax_mode": "kill_after"},
    "W3": {"desc": "after pax.test returned and record_outcome committed (T10), before chip.WorkResult (kill at entry of the 11th write to the db)", "strace": ("write", 11)},
    "W4a": {"desc": "state write between chip.Observation(pax.test) (T9) and record_outcome (T10): kill at entry of the 10th write", "strace": ("write", 10)},
    "W4b": {"desc": "state write between chip.WorkResult (T11) and the terminal checkpoint (T12): kill at entry of the 12th write", "strace": ("write", 12)},
    "W4c": {"desc": "state write of chip.Observation(pax.test) itself (T9 not written; effect already in ledger): kill at entry of the 9th write", "strace": ("write", 9)},
    "W4d": {"desc": "record_intent(pax.test) (T8) written but not yet fdatasync'd: kill at entry of the 8th fdatasync", "strace": ("fdatasync", 8)},
}


def classify(charges, final_status, progress_lost, crashed):
    if crashed:
        return "crashed_on_resume"
    if charges >= 2:
        return "duplicate_effect"
    if final_status != "completed":
        return "stuck"
    if progress_lost:
        return "lost_progress"
    return "correct" if charges == 1 else "lost_progress"


def resume_and_settle(c, rec):
    """Resume in fresh processes; settle OutcomeUncertain the only documented way (operator
    writes the observation, from the ledger), then resume again."""
    time.sleep(1.2)  # lets the 1 s lease of the killed owner expire (contract v1 has no forced takeover)
    c.start_model()
    r1 = c.run(durable=True, tag="resume1")
    rec["resume"] = brief(r1)
    rec["history_after_resume"] = summarize_history(c.history())
    rec["ledger_after_resume"] = len(c.ledger_lines())
    rec["refused_outcome_uncertain"] = r1["exit"] == 3 and "lease" not in r1["stderr"] and "uncertain" in r1["stderr"].lower()
    final = r1
    if rec["refused_outcome_uncertain"]:
        h = c.history()
        open_keys = [i["key"] for i in h["intents"] if i["state"] != "resolved"]
        # Ground truth for the operator: the ledger line written by the killed process.
        executed = rec["ledger_at_kill"] > 0
        rec["manual_intervention"] = {
            "what": "operator wrote chip.Observation for each open intent via sa-state reconcile (no product command exists; same record durability_bench.rs writes)",
            "open_intents": open_keys, "conclusion_from_ledger": "executed" if executed else "not_executed",
            "results": [c.reconcile(k, executed) for k in open_keys],
        }
        r2 = c.run(durable=True, tag="resume2")
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
        rec["outcome_after_manual_resolution"] = classify(len(led), final_status, reads_redone, crashed)
    else:
        rec["outcome"] = classify(len(led), final_status, reads_redone, crashed)
    return rec


def e2_window(env, name, spec):
    c = Case(env, f"e2-{name}")
    c.start_model(kill_on=spec.get("kill_on"))
    wrap = strace_kill(c.db, *spec["strace"]) if "strace" in spec else None
    crash = c.run(durable=True, pax_mode=spec.get("pax_mode", "normal"), wrap=wrap, tag="crash")
    c.stop_model()
    h = c.history()
    sh = summarize_history(h)
    rec = {"kill_point": spec["desc"], "crash_run": brief(crash),
           "kill_reached": crash["killed_by_sigkill"],
           "db_transactions_at_kill": c.db_transactions(),
           "history_at_kill": sh, "ledger_at_kill": len(c.ledger_lines())}
    rec["read_recorded_before_kill"] = bool(sh and any(i[1] == "project.read" and i[2] == "resolved" for i in sh["intents"]))
    return resume_and_settle(c, rec)


def e2(env):
    windows = {}
    for name, spec in WINDOWS.items():
        windows[name] = e2_window(env, name, spec)
        print(name, windows[name]["outcome"], windows[name].get("outcome_after_manual_resolution"),
              windows[name]["charges_in_ledger"], file=sys.stderr)
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
