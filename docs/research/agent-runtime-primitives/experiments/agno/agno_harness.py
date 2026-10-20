"""Agno harness for the agent-runtime-primitives protocol (E1, E2, E3, E5).

Public Agno API only, a scripted fake Model subclass, fake tools, no network.
Run with the venv's python and AGNO_TELEMETRY=false (see README.md).

Subcommands:
  e1                      E1 (in-process, no db) + invalid-args variant
  e2 --out DIR            E2 orchestrator: child processes SIGKILL themselves at each window
  e2-child / e2-resume    internal (spawned by e2)
  e3 --out DIR            E3 orchestrator (team route + confirmation across restart)
  e3-child / e3-resume    internal
  e5                      E5 overhead + instantiation + import time
"""

import argparse
import json
import os
import signal
import sqlite3
import statistics
import subprocess
import sys
import time
import uuid
from typing import Any, Dict, List, Optional

os.environ.setdefault("AGNO_TELEMETRY", "false")

from agno.agent import Agent  # noqa: E402
from agno.models.base import Model  # noqa: E402
from agno.models.message import MessageMetrics  # noqa: E402
from agno.models.response import ModelResponse  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TASK = "Charge account acct-7 the amount lookup('price') returns, then report the charge."
PRICE = 42

# ---------------------------------------------------------------------------
# Side-effect ledger (outside the candidate's store), fsync before returning
# ---------------------------------------------------------------------------
LEDGER = os.environ.get("LEDGER_PATH")
KILL_AT = os.environ.get("KILL_AT", "")
_invocation = {"n": 0}


def ledger_append(tool: str, args: Dict[str, Any], extra: Optional[Dict[str, Any]] = None) -> None:
    _invocation["n"] += 1
    if not LEDGER:
        return
    rec = {"tool": tool, "args": args, "invocation": _invocation["n"], "pid": os.getpid(), "t": time.time()}
    if extra:
        rec.update(extra)
    with open(LEDGER, "a") as f:
        f.write(json.dumps(rec) + "\n")
        f.flush()
        os.fsync(f.fileno())


def die(tag: str) -> None:
    """Deterministic kill point: SIGKILL ourselves (never a graceful signal)."""
    if LEDGER:
        with open(LEDGER + ".events", "a") as f:
            f.write(json.dumps({"event": "sigkill", "at": tag, "pid": os.getpid()}) + "\n")
            f.flush()
            os.fsync(f.fileno())
    os.kill(os.getpid(), signal.SIGKILL)


def charges_in_ledger() -> int:
    if not LEDGER or not os.path.exists(LEDGER):
        return 0
    return sum(1 for line in open(LEDGER) if json.loads(line)["tool"] == "charge")


# ---------------------------------------------------------------------------
# Fake tools. `fc` is Agno's injected FunctionCall (gives call_id); Agno has no
# idempotency-key concept, so we record the call_id + run_id it exposes.
# ---------------------------------------------------------------------------
def lookup(key: str, fc=None) -> str:
    """Look up a value by key.

    Args:
        key: the key to look up
    """
    ledger_append("lookup", {"key": key}, {"call_id": getattr(fc, "call_id", None)})
    return str(PRICE)


def charge(account: str, amount: int, fc=None, run_context=None) -> str:
    """Charge an account.

    Args:
        account: account id
        amount: amount to charge
    """
    ledger_append(
        "charge",
        {"account": account, "amount": amount},
        {
            "call_id": getattr(fc, "call_id", None),
            "run_id": getattr(run_context, "run_id", None),
            "idempotency_key_forwarded": None,
        },
    )
    if KILL_AT in ("W2", "W6b"):
        die(KILL_AT)
    return f"charged {account} {amount}"


def kill_hook(function_name: str, function_call, arguments: Dict[str, Any]):
    """Agent-level tool hook: runs after the model chose the call, before the entrypoint."""
    if KILL_AT == "W1" and function_name == "charge":
        die("W1")
    return function_call(**arguments)


# ---------------------------------------------------------------------------
# Scripted fake model. Reply index = number of assistant messages already in
# the conversation it is handed (= replies made in this process + replies
# recorded in restored state), so a resumed run continues the same script.
# ---------------------------------------------------------------------------
MODEL_CALLS = {"n": 0}
SEEN_INPUTS: List[Any] = []


class ScriptedModel(Model):
    def __init__(self, script: List[Any], name: str = "scripted", on_call=None):
        super().__init__(id=f"fake-{name}", name=name, provider="fake")
        self._script = script
        self._on_call = on_call

    def get_instructions_for_model(self, *a, **k):
        return None

    def get_system_message_for_model(self, *a, **k):
        return None

    def _reply(self, messages) -> ModelResponse:
        MODEL_CALLS["n"] += 1
        idx = sum(1 for m in (messages or []) if getattr(m, "role", None) == "assistant")
        SEEN_INPUTS.append(
            {"model": self.name, "idx": idx, "roles": [getattr(m, "role", None) for m in messages or []],
             "user_contents": [str(getattr(m, "content", ""))[:300] for m in messages or [] if getattr(m, "role", None) == "user"]}
        )
        if self._on_call:
            self._on_call(idx)
        step = self._script[min(idx, len(self._script) - 1)]
        if step[0] == "raise":
            raise RuntimeError(step[1])
        if step[0] == "tool":
            _, name, args = step
            return ModelResponse(
                role="assistant",
                tool_calls=[{"id": f"call_{uuid.uuid4().hex[:12]}", "type": "function",
                             "function": {"name": name, "arguments": json.dumps(args)}}],
                response_usage=MessageMetrics(),
            )
        return ModelResponse(role="assistant", content=step[1], response_usage=MessageMetrics())

    def invoke(self, *a, **k):
        return self._reply(k.get("messages"))

    async def ainvoke(self, *a, **k):
        return self._reply(k.get("messages"))

    def invoke_stream(self, *a, **k):
        yield self._reply(k.get("messages"))

    async def ainvoke_stream(self, *a, **k):
        yield self._reply(k.get("messages"))

    def _parse_provider_response(self, response, **k):
        return response

    def _parse_provider_response_delta(self, response):
        return response


E1_SCRIPT = [
    ("tool", "lookup", {"key": "price"}),
    ("tool", "charge", {"account": "acct-7", "amount": PRICE}),
    ("text", f"Charged acct-7 {PRICE}."),
]


def model_kill_on_final(idx: int) -> None:
    # W3: the 3rd model call happens only after the charge batch's checkpoint
    # callback (after_tool_results) returned, i.e. after the result was recorded.
    if KILL_AT == "W3" and idx == 2:
        die("W3")


def make_agent(db=None, checkpoint=None, confirm=False, tool_hooks=True, script=None, on_call=None):
    from agno.tools import tool

    charge_tool = tool(requires_confirmation=True)(charge) if confirm else charge
    kw: Dict[str, Any] = dict(
        name="payer", id="payer",
        model=ScriptedModel(script or E1_SCRIPT, "payer", on_call=on_call),
        tools=[lookup, charge_tool],
        telemetry=False,
    )
    if tool_hooks:
        kw["tool_hooks"] = [kill_hook]
    if db is not None:
        kw["db"] = db
    if checkpoint is not None:
        kw["checkpoint"] = checkpoint
    return Agent(**kw)


# ---------------------------------------------------------------------------
# E1
# ---------------------------------------------------------------------------
def cmd_e1(args) -> None:
    global LEDGER
    out = args.out
    os.makedirs(out, exist_ok=True)
    LEDGER = os.path.join(out, "e1_ledger.jsonl")
    if os.path.exists(LEDGER):
        os.remove(LEDGER)
    agent = make_agent(tool_hooks=False)
    MODEL_CALLS["n"] = 0
    t0 = time.perf_counter()
    r = agent.run(TASK, session_id="e1")
    wall = (time.perf_counter() - t0) * 1000
    lines = [json.loads(x) for x in open(LEDGER)]
    lookups = [x for x in lines if x["tool"] == "lookup"]
    charges = [x for x in lines if x["tool"] == "charge"]
    success = len(lookups) == 1 and len(charges) == 1 and charges[0]["args"]["amount"] == PRICE
    res: Dict[str, Any] = {
        "success": success, "model_calls": MODEL_CALLS["n"], "tool_calls": len(lines),
        "run_status": str(r.status), "final_content": r.content, "retries": 0, "tokens": None,
        "wall_ms": round(wall, 3),
        "charge_call_id": charges[0].get("call_id") if charges else None,
    }

    # Invalid-args variant: the charge call omits the required `amount`.
    LEDGER = os.path.join(out, "e1_invalid_ledger.jsonl")
    if os.path.exists(LEDGER):
        os.remove(LEDGER)
    bad_script = [
        ("tool", "lookup", {"key": "price"}),
        ("tool", "charge", {"account": "acct-7"}),
        ("tool", "charge", {"account": "acct-7", "amount": PRICE}),
        ("text", f"Charged acct-7 {PRICE}."),
    ]
    agent2 = make_agent(tool_hooks=False, script=bad_script)
    MODEL_CALLS["n"] = 0
    r2 = agent2.run(TASK, session_id="e1-invalid")
    lines2 = [json.loads(x) for x in open(LEDGER)]
    tool_msgs = [m for m in (r2.messages or []) if m.role == "tool"]
    err_msgs = [str(m.content)[:300] for m in tool_msgs if getattr(m, "tool_call_error", False) or "rror" in str(m.content)]
    res["invalid_args"] = {
        "model_calls": MODEL_CALLS["n"],
        "ledger_charges": sum(1 for x in lines2 if x["tool"] == "charge"),
        "tool_error_returned_to_model": err_msgs,
        "run_status": str(r2.status),
        "tools_recorded": [(t.tool_name, t.tool_args, t.tool_call_error) for t in (r2.tools or [])],
    }

    # Unknown tool + non-JSON args variant.
    LEDGER = None
    weird_script = [
        ("tool", "does_not_exist", {"x": 1}),
        ("text", "done"),
    ]
    agent3 = make_agent(tool_hooks=False, script=weird_script)
    r3 = agent3.run(TASK, session_id="e1-unknown")
    res["unknown_tool"] = [str(m.content)[:200] for m in (r3.messages or []) if m.role == "tool"]
    print(json.dumps(res, default=str))


# ---------------------------------------------------------------------------
# DB inspection (raw sqlite, read-only)
# ---------------------------------------------------------------------------
def inspect_db(db_file: str, run_id: str) -> Dict[str, Any]:
    if not os.path.exists(db_file):
        return {"exists": False}
    con = sqlite3.connect(f"file:{db_file}?mode=ro", uri=True)
    tables = [r[0] for r in con.execute("select name from sqlite_master where type='table'")]
    info: Dict[str, Any] = {"exists": True, "tables": tables}
    for t in tables:
        if "run" in t and "schedule" not in t:
            cols = [r[1] for r in con.execute(f"pragma table_info({t})")]
            info[f"{t}_cols"] = cols
            rows = con.execute(f"select * from {t}").fetchall()
            info[f"{t}_rows"] = len(rows)
            for row in rows:
                d = dict(zip(cols, row))
                blob = d.get("run_data") or d.get("data")
                if blob and run_id in str(d.get("run_id", "")):
                    data = json.loads(blob) if isinstance(blob, str) else blob
                    info["run_status"] = data.get("status")
                    info["run_tools"] = [(x.get("tool_name"), x.get("result")) for x in data.get("tools") or []]
                    info["run_last_checkpoint_at_message_index"] = data.get("last_checkpoint_at_message_index")
    con.close()
    return info


# ---------------------------------------------------------------------------
# E2
# ---------------------------------------------------------------------------
def install_commit_killer(db) -> None:
    """W4: SQLAlchemy 'commit' event fires before COMMIT is sent: kill there, on
    the Nth commit after the charge ledger line exists (state write in flight)."""
    from sqlalchemy import event

    target = int(os.environ.get("W4_COMMIT", "1"))
    seen = {"n": 0}

    @event.listens_for(db.db_engine, "commit")
    def _on_commit(conn):  # noqa: ANN001
        if charges_in_ledger() >= 1:
            seen["n"] += 1
            if seen["n"] == target:
                die(f"W4-commit{target}")


def cmd_e2_child(args) -> None:
    from agno.db.sqlite import SqliteDb

    db = SqliteDb(db_file=args.db)
    if KILL_AT == "W4":
        install_commit_killer(db)
    confirm = KILL_AT.startswith("W6")
    agent = make_agent(db=db, checkpoint=args.checkpoint, confirm=confirm, on_call=model_kill_on_final)
    r = agent.run(TASK, session_id=args.session, run_id=args.run_id)
    if KILL_AT.startswith("W6"):
        # Suspended awaiting approval: the run returned PAUSED and was persisted.
        print(json.dumps({"paused_status": str(r.status), "requirements": len(r.requirements or [])}))
        sys.stdout.flush()
        if KILL_AT == "W6":
            die("W6-suspended")
        else:  # W6b: approve in-process, then die inside charge (after ledger fsync)
            for req in r.requirements or []:
                req.confirm()
            agent.continue_run(run_id=args.run_id, session_id=args.session, requirements=r.requirements)
    print(json.dumps({"completed_without_kill": True, "status": str(r.status)}))


def cmd_e2_resume(args) -> None:
    from agno.db.sqlite import SqliteDb

    global KILL_AT
    KILL_AT = ""
    db = SqliteDb(db_file=args.db)
    confirm = args.window.startswith("W6")
    agent = make_agent(db=db, checkpoint=args.checkpoint, confirm=confirm)
    MODEL_CALLS["n"] = 0
    out: Dict[str, Any] = {}
    stored = agent.get_run_output(run_id=args.run_id, session_id=args.session)
    out["stored_status"] = str(stored.status) if stored else None
    out["stored_tools"] = [(t.tool_name, t.result) for t in (stored.tools or [])] if stored else None
    try:
        if stored is None:
            raise LookupError("no stored run")
        reqs = None
        if stored.requirements and any(not q.is_resolved() for q in stored.requirements):
            reqs = stored.requirements
            for q in reqs:
                if not q.is_resolved():
                    q.confirm()
            out["approved_in_new_process"] = len(reqs)
        r = agent.continue_run(run_id=args.run_id, session_id=args.session, requirements=reqs)
        out["resumed"] = True
        out["final_status"] = str(r.status)
        out["final_content"] = r.content
        out["resume_run_id"] = r.run_id
    except Exception as e:  # documented resume API failed
        out["resumed"] = False
        out["resume_error"] = f"{type(e).__name__}: {e}"
        # Manual intervention: re-submit the task from scratch (new run).
        r = agent.run(TASK, session_id=args.session, run_id=args.run_id + "-rerun")
        out["rerun_status"] = str(r.status)
    out["model_calls_in_resume"] = MODEL_CALLS["n"]
    print(json.dumps(out, default=str))


def run_proc(argv: List[str], env: Dict[str, str]) -> Dict[str, Any]:
    p = subprocess.run([sys.executable, "-I", os.path.abspath(__file__)] + argv, env=env,
                       capture_output=True, text=True, timeout=120)
    last = None
    for line in p.stdout.splitlines():
        try:
            last = json.loads(line)
        except Exception:
            pass
    return {"returncode": p.returncode, "out": last, "stderr_tail": p.stderr[-800:]}


def classify(window: str, charges: int, resume: Dict[str, Any], db_after: Dict[str, Any]) -> str:
    o = resume.get("out") or {}
    if resume["returncode"] not in (0,):
        return "crashed_on_resume"
    if not o.get("resumed"):
        return "stuck"
    if charges > 1:
        return "duplicate_effect"
    if charges == 0:
        return "lost_progress"
    if "completed" in str(o.get("final_status", "")).lower():
        return "correct"
    return "stuck"


def cmd_e2(args) -> None:
    base = os.path.join(args.out, "e2")
    os.makedirs(base, exist_ok=True)
    results: Dict[str, Any] = {}
    plan = [("W1", {}), ("W2", {}), ("W3", {}), ("W4", {"W4_COMMIT": "1"}), ("W4", {"W4_COMMIT": "2"}),
            ("W6", {}), ("W6b", {})]
    for checkpoint in args.checkpoints.split(","):
        for window, extra in plan:
            label = window + (f"-commit{extra['W4_COMMIT']}" if "W4_COMMIT" in extra else "")
            d = os.path.join(base, checkpoint, label)
            os.makedirs(d, exist_ok=True)
            for f in os.listdir(d):
                os.remove(os.path.join(d, f))
            env = dict(os.environ, LEDGER_PATH=os.path.join(d, "ledger.jsonl"), KILL_AT=window, **extra)
            dbf = os.path.join(d, "agno.db")
            common = ["--db", dbf, "--session", f"s-{label}", "--run-id", f"r-{label}", "--checkpoint", checkpoint]
            child = run_proc(["e2-child"] + common, env)
            global LEDGER
            LEDGER = env["LEDGER_PATH"]
            charges_before = charges_in_ledger()
            db_before = inspect_db(dbf, f"r-{label}")
            env_r = dict(env, KILL_AT="")
            resume = run_proc(["e2-resume", "--window", window] + common, env_r)
            charges = charges_in_ledger()
            db_after = inspect_db(dbf, f"r-{label}")
            o = resume.get("out") or {}
            stored_charge_results = sum(1 for n, res in (db_after.get("run_tools") or []) if n == "charge" and res)
            rec = {
                "child_returncode": child["returncode"],
                "killed_by_sigkill": child["returncode"] == -9,
                "child_out": child.get("out"),
                "charges_before_resume": charges_before,
                "db_before_resume": {k: db_before.get(k) for k in ("exists", "run_status", "run_tools", "run_last_checkpoint_at_message_index")},
                "resumed": bool(o.get("resumed")),
                "resume": o,
                "resume_returncode": resume["returncode"],
                "resume_stderr_tail": resume["stderr_tail"] if resume["returncode"] else None,
                "charges_in_ledger": charges,
                "db_after_resume": {k: db_after.get(k) for k in ("run_status", "run_tools")},
                "final_state_consistent": None,
                "progress_lost": None,
            }
            # Candidate state vs ledger: the resumed run's recorded charge executions
            # should equal the ledger's charge lines.
            if o.get("resumed"):
                resumed_run = inspect_db(dbf, o.get("resume_run_id") or f"r-{label}")
                rec_charge = sum(1 for n, res in (resumed_run.get("run_tools") or []) if n == "charge" and res)
                rec["recorded_charge_results"] = rec_charge
                rec["final_state_consistent"] = rec_charge == charges
            else:
                rec["recorded_charge_results"] = stored_charge_results
                rec["final_state_consistent"] = False
            # progress lost: lookup recorded before the kill had to be redone?
            led = [json.loads(x) for x in open(env["LEDGER_PATH"])] if os.path.exists(env["LEDGER_PATH"]) else []
            rec["lookups_in_ledger"] = sum(1 for x in led if x["tool"] == "lookup")
            rec["progress_lost"] = rec["lookups_in_ledger"] > 1
            rec["manual_intervention"] = None if o.get("resumed") else "resubmit task as new run (resume API had no row)"
            rec["outcome"] = classify(window, charges, resume, db_after)
            results.setdefault(checkpoint, {})[label] = rec
            print(f"[e2] {checkpoint} {label}: outcome={rec['outcome']} charges={charges} resumed={rec['resumed']}", file=sys.stderr)
    with open(os.path.join(args.out, "e2_raw.json"), "w") as f:
        json.dump(results, f, indent=2, default=str)
    print(json.dumps(results, default=str))


# ---------------------------------------------------------------------------
# E3: Triage (team leader, route mode) -> Specialist -> approval -> restart -> resume
# Also W5 (kill during the handoff) and a Specialist-failure variant.
# ---------------------------------------------------------------------------
def make_team(db, checkpoint, specialist_script=None, kill_in_specialist=False, mode="route"):
    from agno.team import Team
    from agno.team.mode import TeamMode
    from agno.tools import tool

    def spec_on_call(idx):
        if kill_in_specialist and idx == 0:
            die("W5")  # handoff happened (leader chose delegate), specialist has not acted

    specialist = Agent(
        name="Specialist", id="specialist",
        model=ScriptedModel(specialist_script or E1_SCRIPT, "specialist", on_call=spec_on_call),
        tools=[lookup, tool(requires_confirmation=True)(charge)],
        telemetry=False,
    )
    leader = ScriptedModel(
        [("tool", "delegate_task_to_member", {"member_id": "specialist", "task": TASK}),
         ("text", "Specialist handled it.")],
        "triage",
    )
    return Team(name="Triage", id="triage", model=leader, members=[specialist], db=db,
                mode=TeamMode(mode), checkpoint=checkpoint, telemetry=False)


def cmd_e3_child(args) -> None:
    from agno.db.sqlite import SqliteDb

    db = SqliteDb(db_file=args.db)
    team = make_team(db, args.checkpoint, kill_in_specialist=(KILL_AT == "W5"))
    r = team.run(TASK, session_id=args.session, run_id=args.run_id)
    info = {
        "team_status": str(r.status), "team_run_id": r.run_id, "session_id": r.session_id,
        "requirements": [
            {"tool": q.tool_execution.tool_name if q.tool_execution else None,
             "member_agent_id": q.member_agent_id, "member_run_id": q.member_run_id}
            for q in (r.requirements or [])],
        "member_runs": [(m.run_id, getattr(m, "parent_run_id", None), str(m.status)) for m in (r.member_responses or [])],
        "specialist_inputs": [x for x in SEEN_INPUTS if x["model"] == "specialist"],
        "triage_inputs": [x for x in SEEN_INPUTS if x["model"] == "triage"],
    }
    print(json.dumps(info, default=str))
    sys.stdout.flush()
    die("W6-team-suspended")


def cmd_e3_resume(args) -> None:
    from agno.db.sqlite import SqliteDb

    global KILL_AT
    KILL_AT = ""
    db = SqliteDb(db_file=args.db)
    team = make_team(db, args.checkpoint)
    out: Dict[str, Any] = {}
    stored = team.get_run_output(run_id=args.run_id, session_id=args.session)
    out["stored_status"] = str(stored.status) if stored else None
    try:
        if stored is None:
            raise LookupError("no stored team run")
        reqs = stored.requirements or []
        for q in reqs:
            if not q.is_resolved():
                q.confirm()
        out["approved"] = len(reqs)
        r = team.continue_run(run_id=args.run_id, session_id=args.session, requirements=reqs or None)
        out["resumed"] = True
        out["final_status"] = str(r.status)
        out["final_content"] = r.content
        out["member_runs"] = [(m.run_id, getattr(m, "parent_run_id", None), str(m.status)) for m in (r.member_responses or [])]
        out["specialist_inputs_on_resume"] = [x for x in SEEN_INPUTS if x["model"] == "specialist"]
    except Exception as e:
        out["resumed"] = False
        out["resume_error"] = f"{type(e).__name__}: {e}"
    print(json.dumps(out, default=str))


def cmd_e3(args) -> None:
    global LEDGER
    base = os.path.join(args.out, "e3")
    os.makedirs(base, exist_ok=True)
    res: Dict[str, Any] = {}
    for label, kill in (("approval_restart", ""), ("W5_handoff_kill", "W5")):
        d = os.path.join(base, label)
        os.makedirs(d, exist_ok=True)
        for f in os.listdir(d):
            os.remove(os.path.join(d, f))
        env = dict(os.environ, LEDGER_PATH=os.path.join(d, "ledger.jsonl"), KILL_AT=kill)
        common = ["--db", os.path.join(d, "agno.db"), "--session", f"s-{label}", "--run-id", f"t-{label}",
                  "--checkpoint", args.checkpoint]
        child = run_proc(["e3-child"] + common, env)
        LEDGER = env["LEDGER_PATH"]
        before = charges_in_ledger()
        resume = run_proc(["e3-resume"] + common, dict(env, KILL_AT=""))
        res[label] = {
            "child_returncode": child["returncode"], "child_out": child["out"],
            "charges_before_resume": before,
            "resume_returncode": resume["returncode"], "resume": resume["out"],
            "resume_stderr_tail": resume["stderr_tail"] if resume["returncode"] else None,
            "charges_in_ledger": charges_in_ledger(),
        }
        print(f"[e3] {label}: charges={res[label]['charges_in_ledger']} resume={resume['out']}", file=sys.stderr)

    # Specialist failure (in-process): specialist's model raises.
    LEDGER = None
    from agno.db.in_memory import InMemoryDb

    team = make_team(InMemoryDb(), None, specialist_script=[("raise", "specialist model exploded")])
    r = team.run(TASK, session_id="fail")
    res["specialist_failure"] = {
        "team_status": str(r.status), "team_content": r.content,
        "member_runs": [(m.run_id, str(m.status), str(m.content)[:200]) for m in (r.member_responses or [])],
        "tool_results_seen_by_leader": [(t.tool_name, str(t.result)[:200]) for t in (r.tools or [])],
    }
    with open(os.path.join(args.out, "e3_raw.json"), "w") as f:
        json.dump(res, f, indent=2, default=str)
    print(json.dumps(res, default=str))


# ---------------------------------------------------------------------------
# E5
# ---------------------------------------------------------------------------
def pct(xs: List[float]) -> Dict[str, float]:
    xs = sorted(xs)
    q = statistics.quantiles(xs, n=10)
    return {"p10": round(q[0], 4), "median": round(statistics.median(xs), 4), "p90": round(q[-1], 4)}


def cmd_e5(args) -> None:
    global LEDGER
    import tempfile
    import tracemalloc

    from agno.db.sqlite import SqliteDb

    n = args.n
    tmp = tempfile.mkdtemp(prefix="agno-e5-", dir=args.out)
    LEDGER = os.path.join(tmp, "ledger.jsonl")
    res: Dict[str, Any] = {"repetitions": n}

    def bench(agent_factory, label):
        agent = agent_factory()
        agent.run(TASK, session_id=f"{label}-warm")  # warm-up
        times = []
        for i in range(n):
            t0 = time.perf_counter()
            r = agent.run(TASK, session_id=f"{label}-{i}")
            times.append((time.perf_counter() - t0) * 1000)
            assert "completed" in str(r.status).lower(), r.status
        return pct(times)

    res["in_memory_ms"] = bench(lambda: make_agent(tool_hooks=False), "mem")
    res["durable_ms"] = bench(lambda: make_agent(db=SqliteDb(db_file=os.path.join(tmp, "runs.db")), tool_hooks=False), "sqlite")
    res["durable_tool_batch_ms"] = bench(
        lambda: make_agent(db=SqliteDb(db_file=os.path.join(tmp, "tb.db")), checkpoint="tool-batch", tool_hooks=False), "tb")
    res["ledger_lines"] = sum(1 for _ in open(LEDGER))

    # Agent instantiation (2 tools, no db), N=1000 after warm-up, like their claim's shape.
    def inst():
        return Agent(name="x", model=ScriptedModel(E1_SCRIPT), tools=[lookup, charge], telemetry=False)

    for _ in range(50):
        inst()
    ts = []
    for _ in range(1000):
        t0 = time.perf_counter()
        inst()
        ts.append((time.perf_counter() - t0) * 1e6)
    res["instantiation_us"] = pct(ts)
    # Bare Agent() (no model / tools) for comparison with their "construction" row.
    ts2 = []
    for _ in range(1000):
        t0 = time.perf_counter()
        Agent(telemetry=False)
        ts2.append((time.perf_counter() - t0) * 1e6)
    res["instantiation_bare_us"] = pct(ts2)
    # Memory: tracemalloc peak of one construction (after warm-up), 200 samples.
    peaks = []
    for _ in range(200):
        tracemalloc.start()
        tracemalloc.reset_peak()
        a = inst()
        _, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        peaks.append(peak / 1024)
        del a
    res["instantiation_peak_kib"] = pct(peaks)
    # First-run cost after instantiation (deferred tool-schema work is paid here).
    ts3 = []
    for i in range(n):
        a = make_agent(tool_hooks=False)
        t0 = time.perf_counter()
        a.run(TASK, session_id=f"first-{i}")
        ts3.append((time.perf_counter() - t0) * 1000)
    res["first_run_fresh_agent_ms"] = pct(ts3)

    # Cold import: fresh interpreter each time.
    imp = []
    for _ in range(10):
        p = subprocess.run([sys.executable, "-I", "-c",
                            "import time;t=time.perf_counter();import agno.agent;print((time.perf_counter()-t)*1000)"],
                           capture_output=True, text=True, env=dict(os.environ))
        imp.append(float(p.stdout.strip()))
    res["import_agno_agent_ms"] = pct(imp)
    imp2 = []
    for _ in range(10):
        p = subprocess.run([sys.executable, "-I", "-c",
                            "import time;t=time.perf_counter();import agno.agent, agno.team, agno.db.sqlite;print((time.perf_counter()-t)*1000)"],
                           capture_output=True, text=True, env=dict(os.environ))
        imp2.append(float(p.stdout.strip()))
    res["import_agent_team_sqlite_ms"] = pct(imp2)
    print(json.dumps(res))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd")
    ap.add_argument("--out", default=HERE)
    ap.add_argument("--db")
    ap.add_argument("--session")
    ap.add_argument("--run-id")
    ap.add_argument("--checkpoint", default="tool-batch")
    ap.add_argument("--checkpoints", default="tool-batch,runs")
    ap.add_argument("--window")
    ap.add_argument("--n", type=int, default=50)
    args = ap.parse_args()
    {"e1": cmd_e1, "e2": cmd_e2, "e2-child": cmd_e2_child, "e2-resume": cmd_e2_resume,
     "e3": cmd_e3, "e3-child": cmd_e3_child, "e3-resume": cmd_e3_resume, "e5": cmd_e5}[args.cmd](args)


if __name__ == "__main__":
    main()
