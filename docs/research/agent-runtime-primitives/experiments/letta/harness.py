"""Letta Code (local backend) harness for E1, E2 (W1-W3), E5.

Runs the installed `letta` CLI (pip package, bundled Node) headless with
`--backend local`, an isolated HOME per scenario, the scripted fake
OpenAI-compatible model in fake_openai.py, and fake tools in tool.py invoked
through Letta Code's built-in Bash tool. No network model is called.

usage: python3 -I harness.py <letta-bin> <workdir> <out-json>
"""
import json, os, platform, signal, socket, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
LETTA, WORK, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
TASK = "Charge account acct-7 the amount lookup('price') returns, then report the charge."

def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p

class Scenario:
    def __init__(self, name, mode="", kill_at=""):
        self.dir = os.path.join(WORK, name)
        subprocess.run(["rm", "-rf", self.dir]); os.makedirs(self.dir + "/home"); os.makedirs(self.dir + "/cwd")
        self.ledger = self.dir + "/ledger.jsonl"; self.flag = self.dir + "/killed.flag"
        self.pgid_file = self.dir + "/pgid"; self.port = free_port()
        env = dict(os.environ, FAKE_TOOLPY=HERE + "/tool.py", FAKE_LOG=self.dir + "/fake_requests.jsonl",
                   MODE=mode, KILL_AT=kill_at, KILL_FLAG=self.flag, LETTA_PGID_FILE=self.pgid_file)
        self.server = subprocess.Popen([sys.executable, "-I", HERE + "/fake_openai.py", str(self.port)], env=env)
        self.env = dict(os.environ, HOME=self.dir + "/home", LEDGER=self.ledger, KILL_AT=kill_at,
                        KILL_FLAG=self.flag, LETTA_PGID_FILE=self.pgid_file)
        for _ in range(50):
            try: socket.create_connection(("127.0.0.1", self.port), 0.2).close(); break
            except OSError: time.sleep(0.1)
        self.cli(["connect", "openai-compatible", "--base-url", f"http://127.0.0.1:{self.port}/v1", "--api-key", "x"], check=True)
        self.cli(["backend", "local"], check=True)

    def cli(self, args, check=False, timeout=120):
        t0 = time.perf_counter()
        p = subprocess.Popen([LETTA] + args, cwd=self.dir + "/cwd", env=self.env, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True, start_new_session=True)
        with open(self.pgid_file, "w") as f: f.write(str(p.pid))
        try: out, err = p.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL); out, err = p.communicate(); err += "\n[harness timeout]"
        if check and p.returncode != 0: raise RuntimeError(err[-2000:])
        return {"rc": p.returncode, "out": out, "err": err, "ms": (time.perf_counter() - t0) * 1000}

    def run(self, prompt, extra=()):
        return self.cli(["--backend", "local", "-m", "openai-compatible/fake", "-p", prompt,
                         "--output-format", "json", *extra])

    def ledger_lines(self):
        if not os.path.exists(self.ledger): return []
        return [json.loads(l) for l in open(self.ledger) if l.strip()]

    def model_calls(self):
        p = self.dir + "/fake_requests.jsonl"
        return [json.loads(l) for l in open(p)] if os.path.exists(p) else []

    def transcript(self):
        base = self.dir + "/home/.letta/lc-local-backend/conversations"
        rows = []
        if os.path.isdir(base):
            for d in sorted(os.listdir(base)):
                p = os.path.join(base, d, "messages.jsonl")
                if os.path.exists(p):
                    for l in open(p):
                        try: e = json.loads(l)
                        except ValueError: rows.append({"corrupt_line": l[:200]}); continue
                        m = e.get("message") or {}
                        rows.append({"type": e.get("type"), "role": m.get("role"),
                                     "tool_calls": [c.get("arguments", {}).get("command", "")[-30:]
                                                    for c in m.get("content", []) if isinstance(c, dict) and c.get("type") == "toolCall"],
                                     "text": json.dumps(m.get("content"))[:160] if m.get("role") != "assistant" else None})
        return rows

    def close(self):
        self.server.kill(); self.server.wait()

def parse(res):
    try: return json.loads(res["out"])
    except ValueError: return None

def agent_ids(sc):
    d = sc.dir + "/home/.letta/lc-local-backend/agents"
    return os.listdir(d) if os.path.isdir(d) else []

results = {"candidate": "letta (letta-code local backend)", "commit": None, "package": None,
           "environment": {"cpus": os.cpu_count(), "python": platform.python_version(), "os": sys.platform,
                           "machine": platform.machine()}, "blocked": []}
ver = subprocess.run([LETTA, "--version"], capture_output=True, text=True, env=dict(os.environ, HOME=WORK))
results["package"] = "letta==" + ver.stdout.strip().split()[-1] if ver.stdout.strip() else None

# ---------------- E1 ----------------
sc = Scenario("e1")
r = sc.run(TASK, ["--new-agent"]); j = parse(r); led = sc.ledger_lines()
charges = [l for l in led if l["tool"] == "charge"]; lookups = [l for l in led if l["tool"] == "lookup"]
e1 = {"success": len(lookups) == 1 and len(charges) == 1 and charges[0]["args"]["amount"] == "42",
      "model_calls": len(sc.model_calls()), "tool_calls": len(led),
      "final_answer": j and j.get("result"), "reported_usage": j and j.get("usage"),
      "wall_ms": round(r["ms"], 1), "wall_ms_note": "whole CLI process incl. Node cold start", "retries": None}
sc.close()
sc = Scenario("e1_invalid", mode="omit_arg")
r = sc.run(TASK, ["--new-agent"]); j = parse(r); led = sc.ledger_lines()
tr = sc.transcript()
e1["invalid_args_handled"] = {
    "rc": r["rc"], "model_calls": len(sc.model_calls()), "ledger": [l["tool"] for l in led],
    "first_tool_result": next((t["text"] for t in tr if t["role"] == "toolResult"), None),
    "final_answer": j and j.get("result")}
sc.close()
results["e1"] = e1

# ---------------- E2 ----------------
windows = {}
for w in ["W1", "W2", "W3"]:
    sc = Scenario("e2_" + w, kill_at=w)
    first = sc.run(TASK, ["--new-agent"])
    killed = os.path.exists(sc.flag)
    before = sc.transcript(); led_before = len([l for l in sc.ledger_lines() if l["tool"] == "charge"])
    agents = agent_ids(sc)
    # Resume: same agent, its last conversation (documented: `letta -p --agent <id>`)
    conv_dirs = os.listdir(sc.dir + "/home/.letta/lc-local-backend/conversations") if os.path.isdir(sc.dir + "/home/.letta/lc-local-backend/conversations") else []
    agent_json = None
    if agents:
        agent_json = json.load(open(sc.dir + "/home/.letta/lc-local-backend/agents/" + agents[0]))
    aid = agent_json and agent_json.get("id")
    resume = sc.run("Continue the task.", ["--agent", aid] + (["--conversation", "local-conv-1"] if aid else [])) if aid else None
    jr = resume and parse(resume)
    after = sc.transcript(); led = sc.ledger_lines()
    charges = [l for l in led if l["tool"] == "charge"]
    final_charge_results = sum(1 for t in after if t["role"] == "toolResult" and t["text"] and "CHARGED" in t["text"])
    outcome = ("crashed_on_resume" if resume is None or resume["rc"] not in (0,) else
               "duplicate_effect" if len(charges) > 1 else
               "correct" if len(charges) == 1 else "lost_progress")
    windows[w] = {"killed": killed, "first_rc": first["rc"], "agent_persisted": bool(aid),
                  "transcript_before_resume": before, "charges_before_resume": led_before,
                  "resumed": bool(resume and resume["rc"] == 0), "resume_rc": resume and resume["rc"],
                  "resume_stderr_tail": resume and resume["err"][-600:],
                  "resume_answer": jr and jr.get("result"), "charges_in_ledger": len(charges),
                  "charge_results_in_transcript": final_charge_results,
                  "final_state_consistent": final_charge_results == len(charges),
                  "transcript_after_resume": after, "manual_intervention": "re-prompt required (headless resume needs a new user message)",
                  "outcome": outcome}
    sc.close()
windows["W4"] = {"outcome": "not_applicable", "notes": "no public hook between 'about to persist' and 'persisted' in the local store (sync appendFileSync, no fsync)"}
windows["W5"] = {"outcome": None, "notes": "not measured: subagent (Agent tool) kill not scripted; see dossier source analysis"}
windows["W6"] = {"outcome": None, "notes": "not measured: headless default permission mode is 'unrestricted'; durable approval wait exists only in the websocket listener path, not exercised"}
results["e2"] = {"store": "local backend: JSON + JSONL files under ~/.letta/lc-local-backend (no fsync)", "windows": windows}
results["e3"] = {"supported": None, "notes": "not run; see dossier (subagents via Agent tool, approvals via permission modes / listener)"}
results["blocked"].append("E3: not run (multi-agent+approval would need the websocket App Server path)")
results["blocked"].append("E4: no local inference endpoint in this environment (protocol)")

# ---------------- E5 ----------------
sc = Scenario("e5")
r0 = sc.run(TASK, ["--new-agent"])  # warm-up, creates agent
aid = json.load(open(sc.dir + "/home/.letta/lc-local-backend/agents/" + agent_ids(sc)[0]))["id"]
times = []
for i in range(50):
    r = sc.run(TASK, ["--agent", aid, "--new"])
    times.append(r["ms"])
ver_t = []
for i in range(5):
    t0 = time.perf_counter(); subprocess.run([LETTA, "--version"], capture_output=True, env=sc.env); ver_t.append((time.perf_counter() - t0) * 1000)
q = statistics.quantiles(times, n=10)
results["e5"] = {"repetitions": 50, "in_memory_ms": None,
                 "durable_ms": {"p10": round(q[0], 1), "median": round(statistics.median(times), 1), "p90": round(q[-1], 1)},
                 "durable_note": "one full `letta -p` process per run (local file store); includes Node start + agent load, not comparable to in-process harnesses",
                 "cold_start_version_ms_median": round(statistics.median(ver_t), 1),
                 "ledger_charges_total": len([l for l in sc.ledger_lines() if l["tool"] == "charge"])}
sc.close()
json.dump(results, open(OUT, "w"), indent=2)
print(json.dumps({k: results[k] for k in ("e1",)}, indent=1)[:3000])
for w, v in windows.items(): print(w, v.get("outcome"), v.get("charges_in_ledger"), v.get("resume_rc"))
print(results["e5"])
