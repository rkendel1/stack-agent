"""Shared-protocol harness for the OpenAI Agents SDK (openai-agents, Python).

Experiments E1, E2 (W1-W6 + one extra window), E3, E5 from
docs/research/agent-runtime-primitives/protocol.md.

* No network: tracing disabled, a scripted fake ``Model`` subclass, fake tools.
* Ledger: ledger.jsonl outside the SDK's state store, fsynced before a tool returns.
* Kill points: deterministic ``os.kill(os.getpid(), SIGKILL)`` at exact code points.

Run everything:  python -I harness.py all --work <dir> --out <results.json>
"""

from __future__ import annotations

import os

os.environ["OPENAI_AGENTS_DISABLE_TRACING"] = "1"
os.environ.setdefault("OPENAI_API_KEY", "sk-fake-not-used")

import argparse
import asyncio
import json
import platform
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from agents import (
    Agent,
    ModelSettings,
    RunConfig,
    RunHooks,
    Runner,
    RunState,
    SQLiteSession,
    function_tool,
    handoff,
    set_tracing_disabled,
)
from agents.items import ModelResponse
from agents.models.interface import Model
from agents.usage import Usage
from openai.types.responses import (
    ResponseFunctionToolCall,
    ResponseOutputMessage,
    ResponseOutputText,
)

set_tracing_disabled(True)

GOAL = "Charge account acct-7 the amount lookup('price') returns, then report the charge."
PRICE = 42
SELF = os.path.abspath(__file__)

# --------------------------------------------------------------------------------------
# Globals configured per process
# --------------------------------------------------------------------------------------
WORK: Path = Path(".")
KILL_AT: str | None = None  # window name
TOOL_CALLS = 0


def die(tag: str) -> None:
    marker(f"KILL {tag}")
    os.kill(os.getpid(), signal.SIGKILL)


def marker(msg: str) -> None:
    with open(WORK / "events.log", "a") as f:
        f.write(f"{os.getpid()} {msg}\n")
        f.flush()
        os.fsync(f.fileno())


def ledger_write(tool: str, args: dict[str, Any]) -> None:
    global TOOL_CALLS
    TOOL_CALLS += 1
    line = {"tool": tool, "args": args, "invocation": TOOL_CALLS, "pid": os.getpid(), "t": time.time()}
    with open(WORK / "ledger.jsonl", "a") as f:
        f.write(json.dumps(line) + "\n")
        f.flush()
        os.fsync(f.fileno())


def read_ledger(work: Path) -> list[dict[str, Any]]:
    p = work / "ledger.jsonl"
    if not p.exists():
        return []
    return [json.loads(x) for x in p.read_text().splitlines() if x.strip()]


# --------------------------------------------------------------------------------------
# Fake tools
# --------------------------------------------------------------------------------------
@function_tool
def lookup(key: str) -> int:
    """Look up a value by key."""
    ledger_write("lookup", {"key": key})
    return PRICE


def _charge_impl(account: str, amount: int) -> str:
    ledger_write("charge", {"account": account, "amount": amount})
    if KILL_AT == "W2":
        die("W2 inside charge after ledger fsync")
    return f"charged {account} {amount}"


@function_tool
def charge(account: str, amount: int) -> str:
    """Charge an account. No idempotency-key parameter exists in the SDK's tool contract."""
    return _charge_impl(account, amount)


@function_tool(needs_approval=True, name_override="charge")
def charge_approval(account: str, amount: int) -> str:
    """Charge an account (requires human approval)."""
    return _charge_impl(account, amount)


# --------------------------------------------------------------------------------------
# Scripted fake model
# --------------------------------------------------------------------------------------
def fcall(idx: int, name: str, args: str) -> ResponseFunctionToolCall:
    return ResponseFunctionToolCall(
        id=f"fc_{idx}", call_id=f"call_{idx}", type="function_call", name=name, arguments=args
    )


def msg(idx: int, text: str) -> ResponseOutputMessage:
    return ResponseOutputMessage(
        id=f"msg_{idx}",
        type="message",
        role="assistant",
        status="completed",
        content=[ResponseOutputText(text=text, type="output_text", annotations=[], logprobs=[])],
    )


SCRIPTS: dict[str, list[Any]] = {
    "e1": [
        ("call", "lookup", '{"key": "price"}'),
        ("call", "charge", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("text", f"Charged acct-7 {PRICE}."),
    ],
    # invalid-args variants: one bad reply, then the correct call, then final
    "e1_missing_arg": [
        ("call", "lookup", '{"key": "price"}'),
        ("call", "charge", '{"account": "acct-7"}'),
        ("call", "charge", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("text", f"Charged acct-7 {PRICE}."),
    ],
    "e1_bad_json": [
        ("call", "lookup", '{"key": "price"}'),
        ("call", "charge", '{"account": "acct-7", "amount": 4'),
        ("call", "charge", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("text", f"Charged acct-7 {PRICE}."),
    ],
    "e1_unknown_tool": [
        ("call", "lookup", '{"key": "price"}'),
        ("call", "chrage", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("call", "charge", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("text", f"Charged acct-7 {PRICE}."),
    ],
    "e3": [
        ("call", "transfer_to_specialist", "{}"),
        ("call", "lookup", '{"key": "price"}'),
        ("call", "charge", '{"account": "acct-7", "amount": %d}' % PRICE),
        ("text", f"Charged acct-7 {PRICE}."),
    ],
}


class FakeModel(Model):
    """Reply index = calls in this process + replies already recorded in restored state."""

    def __init__(self, script: str, restored: int = 0):
        self.script = SCRIPTS[script]
        self.calls = 0
        self.restored = restored
        self.inputs: list[Any] = []

    async def get_response(self, system_instructions, input, model_settings, tools, output_schema,
                           handoffs, tracing, *, previous_response_id, conversation_id, prompt,
                           **kw):
        idx = self.calls + self.restored
        self.calls += 1
        self.inputs.append(input if isinstance(input, str) else [dict(i) if isinstance(i, dict) else i for i in input])
        marker(f"model_call idx={idx} tools={[t.name for t in tools]} handoffs={[h.tool_name for h in handoffs]}")
        if KILL_AT == "W3" and idx == len(self.script) - 1:
            die("W3 next model call: charge returned and turn recorded")
        kind, *rest = self.script[idx]
        out = [fcall(idx, rest[0], rest[1])] if kind == "call" else [msg(idx, rest[0])]
        return ModelResponse(
            output=out,
            usage=Usage(requests=1, input_tokens=10, output_tokens=5, total_tokens=15),
            response_id=None,
        )

    def stream_response(self, *a, **k):  # pragma: no cover - not used
        raise NotImplementedError


class KillHooks(RunHooks):
    async def on_tool_start(self, context, agent, tool):
        marker(f"tool_start {tool.name}")
        if KILL_AT == "W1" and tool.name == "charge":
            die("W1 before charge invoked")

    async def on_handoff(self, context, from_agent, to_agent):
        marker(f"handoff {from_agent.name}->{to_agent.name}")
        if KILL_AT == "W5":
            die("W5 during handoff")


class KillSession(SQLiteSession):
    """SQLiteSession with an about-to-persist hook for W4."""

    async def add_items(self, items):
        types = [(i.get("type"), i.get("call_id")) if isinstance(i, dict) else str(i) for i in items]
        marker(f"session.add_items about-to-persist {types}")
        if KILL_AT == "W4" and any(
            isinstance(i, dict) and i.get("type") == "function_call_output" and i.get("call_id") == "call_1"
            for i in items
        ):
            die("W4 during state write (charge output batch, before commit)")
        await super().add_items(items)
        marker("session.add_items persisted")


def count_recorded_replies(items: list[Any]) -> int:
    n = 0
    for i in items:
        d = i if isinstance(i, dict) else {}
        if d.get("type") == "function_call" or (d.get("role") == "assistant"):
            n += 1
    return n


def summarize_items(items: list[Any]) -> list[str]:
    out = []
    for i in items:
        d = i if isinstance(i, dict) else {}
        t = d.get("type") or d.get("role")
        if t == "function_call":
            out.append(f"function_call:{d.get('name')}:{d.get('call_id')}")
        elif t == "function_call_output":
            out.append(f"function_call_output:{d.get('call_id')}:{str(d.get('output'))[:60]}")
        elif d.get("role") == "user":
            c = d.get("content")
            out.append(f"user:{str(c)[:40]}")
        elif d.get("role") == "assistant":
            out.append("assistant_message")
        else:
            out.append(str(t))
    return out


def e1_agent(tool=charge) -> Agent:
    return Agent(name="payer", instructions="Do the task.", tools=[lookup, tool])


# --------------------------------------------------------------------------------------
# Subcommands (each runs in its own process)
# --------------------------------------------------------------------------------------
async def cmd_e1(script: str) -> dict[str, Any]:
    model = FakeModel(script)
    agent = Agent(name="payer", instructions="Do the task.", tools=[lookup, charge], model=model)
    t0 = time.perf_counter()
    err = None
    try:
        res = await Runner.run(agent, GOAL, run_config=RunConfig(tracing_disabled=True), hooks=KillHooks())
        final = res.final_output
        usage = res.context_wrapper.usage
        items = summarize_items(res.to_input_list())
        tokens = {"input": usage.input_tokens, "output": usage.output_tokens, "total": usage.total_tokens, "requests": usage.requests}
    except Exception as e:  # noqa: BLE001
        err = f"{type(e).__name__}: {e}"
        final, items, tokens = None, None, None
    wall = (time.perf_counter() - t0) * 1000
    tool_outputs_seen = []
    for inp in model.inputs:
        if isinstance(inp, list):
            for i in inp:
                if isinstance(i, dict) and i.get("type") == "function_call_output":
                    tool_outputs_seen.append((i.get("call_id"), str(i.get("output"))[:200]))
    return {"final_output": final, "error": err, "model_calls": model.calls, "tool_calls": TOOL_CALLS,
            "tokens": tokens, "wall_ms": round(wall, 3), "items": items,
            "tool_outputs_seen_by_model": sorted(set(tool_outputs_seen))}


async def cmd_e2_run(window: str) -> None:
    global KILL_AT
    KILL_AT = window
    session = KillSession("e2", str(WORK / "session.db"))
    model = FakeModel("e1")
    agent = Agent(name="payer", instructions="Do the task.", tools=[lookup, charge], model=model)
    res = await Runner.run(agent, GOAL, session=session, hooks=KillHooks())
    marker(f"finished without kill: {res.final_output}")


async def cmd_e2_resume(window: str) -> dict[str, Any]:
    session = SQLiteSession("e2", str(WORK / "session.db"))
    before = await session.get_items()
    restored = count_recorded_replies(before)
    model = FakeModel("e1", restored=restored)
    agent = Agent(name="payer", instructions="Do the task.", tools=[lookup, charge], model=model)
    # No "resume run" API exists for a crashed run: the documented way to continue is a new
    # Runner.run on the same session. It requires new input; we pass a continuation message.
    err = None
    try:
        res = await Runner.run(agent, "continue", session=session, hooks=KillHooks())
        final = res.final_output
    except Exception as e:  # noqa: BLE001
        err, final = f"{type(e).__name__}: {e}", None
    after = await session.get_items()
    return {"restored_replies": restored, "session_before": summarize_items(before),
            "session_after": summarize_items(after), "final_output": final, "error": err,
            "model_calls_in_resume": model.calls}


def e3_agents(model: FakeModel) -> tuple[Agent, Agent]:
    specialist = Agent(name="specialist", instructions="Charge.", tools=[lookup, charge_approval], model=model)
    triage = Agent(name="triage", instructions="Route.", handoffs=[handoff(specialist)], model=model)
    return triage, specialist


async def cmd_e3_p1(kill: str | None) -> dict[str, Any]:
    """Process 1: run until approval interruption, persist RunState JSON, optionally die."""
    global KILL_AT
    KILL_AT = kill
    session = KillSession("e3", str(WORK / "session.db"))
    model = FakeModel("e3")
    triage, _ = e3_agents(model)
    res = await Runner.run(triage, GOAL, session=session, hooks=KillHooks())
    interruptions = res.interruptions
    state = res.to_state()
    s = state.to_string()
    tmp = WORK / "state.json.tmp"
    tmp.write_text(s)
    with open(tmp) as f:
        os.fsync(f.fileno())
    os.replace(tmp, WORK / "state.json")
    marker(f"suspended interruptions={[ (i.tool_name, i.agent.name) for i in interruptions]}")
    info = {
        "last_agent": res.last_agent.name,
        "interruptions": [{"tool": i.tool_name, "agent": i.agent.name,
                            "call_id": getattr(i.raw_item, "call_id", None) or (i.raw_item.get("call_id") if isinstance(i.raw_item, dict) else None)}
                           for i in interruptions],
        "state_bytes": len(s),
        "state_top_keys": sorted(json.loads(s).keys()),
        "session_items_at_suspend": summarize_items(await session.get_items()),
        "model_inputs": [summarize_items(i) if isinstance(i, list) else i for i in model.inputs],
    }
    if KILL_AT == "W6":
        (WORK / "p1.json").write_text(json.dumps(info, indent=1, default=str))
        die("W6 while suspended awaiting approval")
    return info


async def cmd_e3_p2(kill: str | None) -> dict[str, Any]:
    """Process 2 (fresh): load RunState, approve, resume."""
    global KILL_AT
    KILL_AT = kill
    session = SQLiteSession("e3", str(WORK / "session.db"))
    raw = (WORK / "state.json").read_text()
    js = json.loads(raw)
    model = FakeModel("e3")
    triage, specialist = e3_agents(model)
    state = await RunState.from_string(triage, raw)
    # restored replies = model responses recorded in the snapshot
    restored = len(js.get("modelResponses") or js.get("model_responses") or [])
    model.restored = restored
    ints = state.get_interruptions()
    for it in ints:
        state.approve(it)
    err = None
    try:
        res = await Runner.run(triage, state, session=session, hooks=KillHooks())
        final, last = res.final_output, res.last_agent.name
    except Exception as e:  # noqa: BLE001
        err, final, last = f"{type(e).__name__}: {e}", None, None
    return {"restored_model_responses": restored,
            "interruptions_after_load": [(i.tool_name, i.agent.name) for i in ints],
            "final_output": final, "last_agent": last, "error": err,
            "model_calls_in_resume": model.calls,
            "session_after": summarize_items(await session.get_items()),
            "model_inputs": [summarize_items(i) if isinstance(i, list) else i for i in model.inputs]}


async def cmd_e3_session_resume() -> dict[str, Any]:
    """Fresh process after a kill with no RunState: continue from SQLiteSession only."""
    session = SQLiteSession("e3", str(WORK / "session.db"))
    before = await session.get_items()
    model = FakeModel("e3", restored=count_recorded_replies(before))
    triage, _ = e3_agents(model)
    err, final, last, agents_called = None, None, None, []
    try:
        res = await Runner.run(triage, "continue", session=session, hooks=KillHooks())
        if res.interruptions:
            st = res.to_state()
            for it in st.get_interruptions():
                st.approve(it)
            res = await Runner.run(triage, st, session=session, hooks=KillHooks())
        final, last = res.final_output, res.last_agent.name
    except Exception as e:  # noqa: BLE001
        err = f"{type(e).__name__}: {e}"
    return {"restored_replies": model.restored, "session_before": summarize_items(before),
            "session_after": summarize_items(await session.get_items()), "final_output": final,
            "last_agent": last, "error": err, "model_calls_in_resume": model.calls,
            "events": (WORK / "events.log").read_text().splitlines()[-30:]}


async def cmd_e3_fail() -> dict[str, Any]:
    """Specialist fails: its charge tool raises after approval -> what does the run do?"""
    @function_tool(needs_approval=True, name_override="charge")
    def bad_charge(account: str, amount: int) -> str:
        """Charge (always fails)."""
        raise RuntimeError("payment backend down")

    model = FakeModel("e3")
    specialist = Agent(name="specialist", instructions="Charge.", tools=[lookup, bad_charge], model=model)
    triage = Agent(name="triage", instructions="Route.", handoffs=[handoff(specialist)], model=model)
    res = await Runner.run(triage, GOAL)
    state = res.to_state()
    for it in state.get_interruptions():
        state.approve(it)
    res2 = await Runner.run(triage, state)
    outs = [summarize_items([i])[0] for i in res2.to_input_list() if isinstance(i, dict) and i.get("type") == "function_call_output"]
    return {"final_output": res2.final_output, "last_agent": res2.last_agent.name, "tool_outputs": outs}


async def cmd_e5(n: int) -> dict[str, Any]:
    async def one(session):
        model = FakeModel("e1")
        agent = Agent(name="payer", instructions="Do the task.", tools=[lookup, charge], model=model)
        t0 = time.perf_counter()
        r = await Runner.run(agent, GOAL, session=session)
        assert r.final_output
        return (time.perf_counter() - t0) * 1000

    def stats(xs):
        xs = sorted(xs)
        q = statistics.quantiles(xs, n=10)
        return {"p10": round(q[0], 3), "median": round(statistics.median(xs), 3), "p90": round(q[-1], 3)}

    await one(None)
    mem = [await one(None) for _ in range(n)]
    dbp = WORK / "e5.db"
    await one(SQLiteSession("warm", str(dbp)))
    dur = [await one(SQLiteSession(f"s{i}", str(dbp))) for i in range(n)]
    memsess = [await one(SQLiteSession(f"m{i}")) for i in range(n)]
    return {"repetitions": n, "in_memory_ms": stats(mem), "durable_ms": stats(dur),
            "sqlite_memory_session_ms": stats(memsess)}


# --------------------------------------------------------------------------------------
# Orchestration
# --------------------------------------------------------------------------------------
def sub(work: Path, *args: str) -> tuple[int, dict | None]:
    work.mkdir(parents=True, exist_ok=True)
    p = subprocess.run([sys.executable, "-I", SELF, *args, "--work", str(work)], capture_output=True, text=True)
    out = None
    for line in p.stdout.splitlines():
        if line.startswith("RESULT "):
            out = json.loads(line[7:])
    if p.returncode not in (0, -9):
        (work / "stderr.log").write_text(p.stderr[-4000:])
    return p.returncode, out


def charges(work: Path) -> int:
    return sum(1 for x in read_ledger(work) if x["tool"] == "charge")


def lookups(work: Path) -> int:
    return sum(1 for x in read_ledger(work) if x["tool"] == "lookup")


def run_all(work: Path, out: Path) -> None:
    import shutil
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    import agents as _a
    results: dict[str, Any] = {
        "candidate": "openai-agents",
        "commit": "64cd5dfc5a1579870dcbed0ea42b27aa84033fee",
        "package": f"openai-agents=={_a.__version__}",
        "environment": {"cpus": os.cpu_count(), "python": platform.python_version(),
                        "os": platform.system().lower(), "machine": platform.machine(),
                        "platform": platform.platform()},
    }
    # ---- E1
    rc, e1 = sub(work / "e1", "e1", "--script", "e1")
    led = read_ledger(work / "e1")
    ok = (lookups(work / "e1") == 1 and charges(work / "e1") == 1
          and [x for x in led if x["tool"] == "charge"][0]["args"]["amount"] == PRICE)
    inv = {}
    for s in ("e1_missing_arg", "e1_bad_json", "e1_unknown_tool"):
        rc2, r = sub(work / s, "e1", "--script", s)
        inv[s] = {"exit": rc2, "error": r and r["error"], "final_output": r and r["final_output"],
                  "model_calls": r and r["model_calls"], "ledger_charges": charges(work / s),
                  "tool_outputs_seen_by_model": r and r["tool_outputs_seen_by_model"]}
    results["e1"] = {"success": ok, "model_calls": e1["model_calls"], "tool_calls": e1["tool_calls"],
                     "ledger": {"lookup": lookups(work / "e1"), "charge": charges(work / "e1")},
                     "invalid_args_handled": inv, "retries": 0, "tokens": e1["tokens"],
                     "wall_ms": e1["wall_ms"], "final_output": e1["final_output"]}
    # ---- E2 (SQLiteSession) W1-W4
    windows: dict[str, Any] = {}
    for w in ("W1", "W2", "W3", "W4"):
        wd = work / f"e2_{w}"
        rc_run, _ = sub(wd, "e2-run", "--window", w)
        killed = rc_run == -9
        rc_res, r = sub(wd, "e2-resume", "--window", w)
        c = charges(wd)
        sess_outputs = sum(1 for x in (r or {}).get("session_after", []) if x.startswith("function_call_output:call_1") or x.startswith("function_call_output:call_2"))
        charge_outputs = [x for x in (r or {}).get("session_after", []) if "charged" in x]
        resumed = bool(r and r["final_output"])
        outcome = ("crashed_on_resume" if rc_res != 0 else
                   "duplicate_effect" if c > 1 else
                   "correct" if c == 1 and resumed else "stuck")
        windows[w] = {"killed": killed, "resumed": resumed, "charges_in_ledger": c,
                      "lookups_in_ledger": lookups(wd),
                      "final_state_consistent": len(charge_outputs) == c,
                      "session_charge_outputs": len(charge_outputs),
                      "progress_lost": lookups(wd) > 1 or c > 1, "manual_intervention": "caller must detect the incomplete run and call Runner.run(agent, <new input>, session=same) — no resume-from-session API; a continuation user message is appended to history",
                      "outcome": outcome, "resume_detail": r}
    # ---- W5: handoff kill (E3 flow, session) then resume via session (no RunState exists yet)
    wd = work / "e2_W5"
    rc_run, _ = sub(wd, "e3-p1", "--kill", "W5")
    rc_res, r = sub(wd, "e3-session-resume")
    c = charges(wd)
    windows["W5"] = {"killed": rc_run == -9, "resumed": bool(r and r.get("final_output")),
                     "charges_in_ledger": c, "lookups_in_ledger": lookups(wd),
                     "final_state_consistent": bool(r) and sum(1 for x in r["session_after"] if "charged" in x) == c,
                     "state_json_exists": (wd / "state.json").exists(),
                     "progress_lost": lookups(wd) > 1 or c > 1,
                     "manual_intervention": "no RunState exists before the first interruption; caller re-runs the entry agent on the session with a continuation input",
                     "outcome": ("crashed_on_resume" if rc_res != 0 else "duplicate_effect" if c > 1 else "correct" if c == 1 and r and r.get("final_output") else "stuck"),
                     "resume_detail": r}
    # ---- W6: killed while suspended; fresh process loads RunState, approves, resumes
    wd = work / "e2_W6"
    rc_run, _ = sub(wd, "e3-p1", "--kill", "W6")
    rc_res, r = sub(wd, "e3-p2", "--kill", "none")
    c = charges(wd)
    windows["W6"] = {"killed": rc_run == -9, "resumed": bool(r and r["final_output"]),
                     "charges_in_ledger": c, "lookups_in_ledger": lookups(wd),
                     "final_state_consistent": bool(r) and sum(1 for x in r["session_after"] if "charged" in x) == c,
                     "progress_lost": lookups(wd) > 1 or c > 1,
                     "manual_intervention": "approve the restored interruption (the intended HITL step); nothing else",
                     "outcome": ("crashed_on_resume" if rc_res != 0 else "duplicate_effect" if c > 1 else "correct" if c == 1 and r and r["final_output"] else "stuck"),
                     "resume_detail": r}
    # ---- extra: W2 after approval — RunState is only a snapshot at the interruption
    wd = work / "e2_W2_after_approval"
    sub(wd, "e3-p1", "--kill", "none")
    rc_p2, _ = sub(wd, "e3-p2", "--kill", "W2")
    rc_p3, r = sub(wd, "e3-p2", "--kill", "none")
    c = charges(wd)
    windows["W2_after_approval"] = {"killed": rc_p2 == -9, "resumed": bool(r and r["final_output"]),
                                    "charges_in_ledger": c, "lookups_in_ledger": lookups(wd),
                                    "progress_lost": c > 1,
                                    "final_state_consistent": bool(r) and sum(1 for x in r["session_after"] if "charged" in x) == c,
                                    "manual_intervention": "re-load the same (only) saved RunState and approve again",
                                    "outcome": ("crashed_on_resume" if rc_p3 != 0 else "duplicate_effect" if c > 1 else "correct" if c == 1 else "stuck"),
                                    "resume_detail": r}
    results["e2"] = {"store": "sqlite (SQLiteSession file, WAL) for W1-W5; RunState.to_string() JSON file for W6",
                     "windows": windows}
    # ---- E3 happy path (two processes, no kill)
    wd = work / "e3"
    _, p1 = sub(wd, "e3-p1", "--kill", "none")
    _, p2 = sub(wd, "e3-p2", "--kill", "none")
    _, fail = sub(work / "e3_fail", "e3-fail")
    results["e3"] = {"supported": True, "p1": p1, "p2": p2, "charges_in_ledger": charges(wd),
                     "lookups_in_ledger": lookups(wd), "specialist_failure": fail}
    # ---- E5
    _, e5 = sub(work / "e5", "e5", "--n", "50")
    imp = []
    for _ in range(5):
        t0 = time.perf_counter()
        subprocess.run([sys.executable, "-I", "-c", "import agents"], check=True)
        imp.append((time.perf_counter() - t0) * 1000)
    base = []
    for _ in range(5):
        t0 = time.perf_counter()
        subprocess.run([sys.executable, "-I", "-c", "pass"], check=True)
        base.append((time.perf_counter() - t0) * 1000)
    e5["cold_start_import_ms"] = {"median_import_agents_process": round(statistics.median(imp), 1),
                                  "median_bare_python_process": round(statistics.median(base), 1)}
    e5["note"] = "fake tools fsync the ledger in both modes; durable = SQLiteSession on a file (WAL)"
    results["e5"] = e5
    results["blocked"] = ["E4: no local inference endpoint reachable in this environment"]
    logs = out.parent / "logs"
    logs.mkdir(exist_ok=True)
    for d in sorted(work.iterdir()):
        for name in ("events.log", "ledger.jsonl"):
            if (d / name).exists():
                (logs / f"{d.name}.{name}").write_text((d / name).read_text())
    out.write_text(json.dumps(results, indent=2, default=str))
    print(json.dumps({k: results[k] for k in ("e1",)}, default=str)[:400])


def main() -> None:
    global WORK
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd")
    ap.add_argument("--work", default=".")
    ap.add_argument("--out", default="results.json")
    ap.add_argument("--script", default="e1")
    ap.add_argument("--window", default=None)
    ap.add_argument("--kill", default="none")
    ap.add_argument("--n", type=int, default=50)
    a = ap.parse_args()
    WORK = Path(a.work)
    WORK.mkdir(parents=True, exist_ok=True)
    kill = None if a.kill == "none" else a.kill
    r: Any = None
    if a.cmd == "all":
        run_all(WORK, Path(a.out))
        return
    if a.cmd == "e1":
        r = asyncio.run(cmd_e1(a.script))
    elif a.cmd == "e2-run":
        asyncio.run(cmd_e2_run(a.window))
    elif a.cmd == "e2-resume":
        r = asyncio.run(cmd_e2_resume(a.window))
    elif a.cmd == "e3-p1":
        r = asyncio.run(cmd_e3_p1(kill))
    elif a.cmd == "e3-p2":
        r = asyncio.run(cmd_e3_p2(kill))
    elif a.cmd == "e3-session-resume":
        r = asyncio.run(cmd_e3_session_resume())
    elif a.cmd == "e3-fail":
        r = asyncio.run(cmd_e3_fail())
    elif a.cmd == "e5":
        r = asyncio.run(cmd_e5(a.n))
    if r is not None:
        print("RESULT " + json.dumps(r, default=str))


if __name__ == "__main__":
    main()
