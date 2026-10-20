"""smolagents harness for the agent-runtime-primitives protocol (E1, E2, E3, E5).

Uses only smolagents' public API, a scripted fake Model and fake tools. No network.
Run with the research venv's python; see README.md for exact commands.

Subcommands:
  e1            run all E1 variants (ToolCallingAgent and CodeAgent)
  e2            run E2 windows (spawns children that SIGKILL themselves)
  e2-child W    internal: run the task and kill at window W
  e2-rerun      internal: fresh-process "resume" (smolagents has no resume API: rerun)
  e3            managed-agent delegation, specialist failure, approval/interrupt probes
  e5            overhead, N=50 after 1 warm-up, plus import time
  all           e1, e2, e3, e5 and write results.json
"""

from __future__ import annotations

import json
import os
import platform
import re
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOGS = HERE / "logs"
LOGS.mkdir(exist_ok=True)

import smolagents  # noqa: E402
from smolagents import CodeAgent, Tool, ToolCallingAgent  # noqa: E402
from smolagents.agents import ActionStep  # noqa: E402
from smolagents.memory import ToolCall  # noqa: E402
from smolagents.models import (  # noqa: E402
    ChatMessage,
    ChatMessageToolCall,
    ChatMessageToolCallFunction,
    MessageRole,
    Model,
)
from smolagents.monitoring import LogLevel  # noqa: E402

TASK = "Charge account `acct-7` the amount `lookup('price')` returns, then report the charge."
PRICE = 12.5
COMMIT = "96f33faaf028479119ec8d34507b47694cf14e34"

# --------------------------------------------------------------------------- ledger + tools

LEDGER: Path | None = None
KILL_AT: str | None = None
_invocation = 0


def ledger_append(tool: str, args: dict) -> None:
    global _invocation
    _invocation += 1
    line = json.dumps({"tool": tool, "args": args, "invocation": _invocation, "pid": os.getpid(), "t": time.time()})
    fd = os.open(LEDGER, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, (line + "\n").encode())
        os.fsync(fd)
    finally:
        os.close(fd)


def read_ledger(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(x) for x in path.read_text().splitlines() if x.strip()]


class LookupTool(Tool):
    name = "lookup"
    description = "Look up a value by key. Returns a number."
    inputs = {"key": {"type": "string", "description": "The key to look up."}}
    output_type = "number"

    def forward(self, key):
        ledger_append("lookup", {"key": key})
        return PRICE


class ChargeTool(Tool):
    name = "charge"
    description = "Charge an account the given amount. Consequential effect."
    inputs = {
        "account": {"type": "string", "description": "Account id."},
        "amount": {"type": "number", "description": "Amount to charge."},
    }
    output_type = "string"

    def forward(self, account, amount):
        ledger_append("charge", {"account": account, "amount": amount})
        if KILL_AT == "W2":
            os.kill(os.getpid(), signal.SIGKILL)  # after the ledger fsync, before returning
        return f"charged {account} {amount}"


# --------------------------------------------------------------------------- scripted fake model


def last_text(messages) -> str:
    if not messages:
        return ""
    m = messages[-1]
    content = m.content if isinstance(m, ChatMessage) else m.get("content")
    if isinstance(content, list):
        return "\n".join(c.get("text", "") for c in content if isinstance(c, dict))
    return str(content or "")


def all_text(messages) -> str:
    out = []
    for m in messages:
        content = m.content if isinstance(m, ChatMessage) else m.get("content")
        if isinstance(content, list):
            out.extend(c.get("text", "") for c in content if isinstance(c, dict))
        else:
            out.append(str(content or ""))
    return "\n".join(out)


def observed_price(messages) -> float | None:
    """The fake model reads the price from the observations it was shown (data-dependent, not hardcoded)."""
    found = re.findall(r"Observation:\n(?:Execution logs:\n)?([0-9]+(?:\.[0-9]+)?)", all_text(messages))
    return float(found[-1]) if found else None


class ScriptedModel(Model):
    """Deterministic script of replies, indexed by how many times the model was called in this process
    (smolagents restores no state, so the 'replies already recorded in restored state' term is always 0)."""

    def __init__(self, script, name="scripted-fake"):
        super().__init__(model_id=name)
        self.script = script
        self.calls = 0
        self.seen: list[str] = []  # last message text shown at each call (to see how errors are surfaced)

    def generate(self, messages, stop_sequences=None, response_format=None, tools_to_call_from=None, **kwargs):
        i = self.calls
        self.calls += 1
        self.seen.append(last_text(messages))
        if i >= len(self.script):
            raise RuntimeError(f"script exhausted at call {i}")
        return self.script[i](messages)


def tc(name, args_fn, call_id):
    def build(messages):
        args = args_fn(messages) if callable(args_fn) else args_fn
        return ChatMessage(
            role=MessageRole.ASSISTANT,
            content="",
            tool_calls=[
                ChatMessageToolCall(
                    id=call_id, type="function", function=ChatMessageToolCallFunction(name=name, arguments=args)
                )
            ],
        )

    return build


def text(content_fn):
    def build(messages):
        c = content_fn(messages) if callable(content_fn) else content_fn
        return ChatMessage(role=MessageRole.ASSISTANT, content=c)

    return build


def charge_args(messages):
    return {"account": "acct-7", "amount": observed_price(messages)}


def final_args(messages):
    return {"answer": f"Charged acct-7 {observed_price_or_charge(messages)}"}


def observed_price_or_charge(messages):
    m = re.findall(r"charged acct-7 ([0-9.]+)", all_text(messages))
    return m[-1] if m else None


# Scripts ---------------------------------------------------------------------------------------

TC_NATIVE = [
    tc("lookup", {"key": "price"}, "c1"),
    tc("charge", charge_args, "c2"),
    tc("final_answer", final_args, "c3"),
]


def _text_call(name, args_fn):
    return text(lambda m: "Action:\n" + json.dumps({"name": name, "arguments": args_fn(m) if callable(args_fn) else args_fn}))


TC_TEXT = [  # model without native tool calling: JSON in the text content
    _text_call("lookup", {"key": "price"}),
    _text_call("charge", charge_args),
    _text_call("final_answer", final_args),
]

TC_MISSING_ARG = [
    tc("lookup", {"key": "price"}, "c1"),
    tc("charge", {"account": "acct-7"}, "c2"),  # omits required `amount`
    tc("charge", charge_args, "c3"),
    tc("final_answer", final_args, "c4"),
]

TC_WRONG_TYPE = [
    tc("lookup", {"key": "price"}, "c1"),
    tc("charge", {"account": "acct-7", "amount": "twelve"}, "c2"),
    tc("charge", charge_args, "c3"),
    tc("final_answer", final_args, "c4"),
]

TC_BAD_JSON = [
    tc("lookup", {"key": "price"}, "c1"),
    text('Action:\n{"name": "charge", "arguments": {"account": "acct-7", "amount": }}'),
    _text_call("charge", charge_args),
    _text_call("final_answer", final_args),
]

TC_PREMATURE_FINAL = [  # used with final_answer_checks
    tc("final_answer", {"answer": "Charged acct-7 12.5"}, "c0"),
    tc("lookup", {"key": "price"}, "c1"),
    tc("charge", charge_args, "c2"),
    tc("final_answer", final_args, "c3"),
]

TC_NEVER_FINISHES = [tc("lookup", {"key": "price"}, f"c{i}") for i in range(2)] + [
    text("I think the account was charged 12.5.")  # provide_final_answer() call after max_steps
]


def code(body):
    return text(f"Thought: next action.\n<code>\n{body}\n</code>")


CA_3STEP = [
    code('price = lookup(key="price")\nprint(price)'),
    code('result = charge(account="acct-7", amount=price)\nprint(result)'),  # `price` persists in interpreter state
    code('final_answer(f"Charged acct-7 {price}")'),
]

CA_1BLOB = [
    code('price = lookup(key="price")\nresult = charge(account="acct-7", amount=price)\nfinal_answer(f"Charged acct-7 {price}")'),
]

CA_MISSING_ARG = [
    code('price = lookup(key="price")\nprint(price)'),
    code('result = charge(account="acct-7")\nprint(result)'),
    code('result = charge(account="acct-7", amount=price)\nprint(result)'),
    code('final_answer(f"Charged acct-7 {price}")'),
]

CA_WRONG_TYPE = [
    code('price = lookup(key="price")\nprint(price)'),
    code('result = charge(account="acct-7", amount="twelve")\nprint(result)'),
    code('final_answer("Charged acct-7 twelve")'),
]

CA_PARTIAL_EFFECT = [  # blob performs the effect, then fails later in the same blob; scripted model re-sends the blob
    code('price = lookup(key="price")\nresult = charge(account="acct-7", amount=price)\nreport = undefined_name\nfinal_answer(report)'),
    code('price = lookup(key="price")\nresult = charge(account="acct-7", amount=price)\nfinal_answer(f"Charged acct-7 {price}")'),
]

CA_NO_CODE = [
    text("I will now charge the account acct-7 for the price."),  # no code block at all
    code('price = lookup(key="price")\nresult = charge(account="acct-7", amount=price)\nfinal_answer(f"Charged acct-7 {price}")'),
]


# --------------------------------------------------------------------------- helpers


def make_agent(kind, script, **kw):
    model = ScriptedModel(list(script))
    tools = [LookupTool(), ChargeTool()]
    if kind == "tool_calling":
        agent = ToolCallingAgent(tools=tools, model=model, verbosity_level=LogLevel.OFF, **kw)
    else:
        agent = CodeAgent(tools=tools, model=model, verbosity_level=LogLevel.OFF, **kw)
    return agent, model


def summarize_steps(agent):
    steps = [s for s in agent.memory.steps if isinstance(s, ActionStep)]
    return [
        {
            "step": s.step_number,
            "tool_calls": [tc.name for tc in (s.tool_calls or [])],
            "error": (type(s.error).__name__ + ": " + str(s.error)[:300]) if s.error else None,
            "observations": (s.observations or "")[:200] or None,
        }
        for s in steps
    ]


def run_case(name, kind, script, **agent_kw):
    global LEDGER, KILL_AT
    KILL_AT = None
    LEDGER = LOGS / f"e1_{name}.ledger.jsonl"
    LEDGER.unlink(missing_ok=True)
    agent, model = make_agent(kind, script, **agent_kw)
    t0 = time.perf_counter()
    err = None
    result = None
    try:
        result = agent.run(TASK, return_full_result=True)
    except Exception as e:  # noqa: BLE001
        err = f"{type(e).__name__}: {e}"
    wall_ms = (time.perf_counter() - t0) * 1000
    led = read_ledger(LEDGER)
    lookups = [x for x in led if x["tool"] == "lookup"]
    charges = [x for x in led if x["tool"] == "charge"]
    verified = len(lookups) == 1 and len(charges) == 1 and charges[0]["args"].get("amount") == PRICE
    steps = summarize_steps(agent)
    return {
        "agent": kind,
        "verified": verified,
        "output": None if result is None else str(result.output),
        "run_state": None if result is None else result.state,
        "raised": err,
        "model_calls": model.calls,
        "tool_calls_in_ledger": len(led),
        "lookups": len(lookups),
        "charges": [c["args"] for c in charges],
        "error_steps": sum(1 for s in steps if s["error"]),
        "steps": steps,
        "model_saw_after_each_call": [s[:400] for s in model.seen[1:]],
        "framework_retries": 0,
        "tokens": None,
        "wall_ms": round(wall_ms, 2),
    }


# --------------------------------------------------------------------------- E1


def charge_was_observed(final_answer, memory, agent=None):
    """final_answer_checks hook: refuse completion unless a charge observation exists."""
    for s in memory.steps:
        if isinstance(s, ActionStep) and s.observations and "charged acct-7" in s.observations:
            return True
    raise AssertionError("no charge observation recorded yet")


def e1():
    cases = {
        "tc_native": ("tool_calling", TC_NATIVE, {}),
        "tc_text_toolcall": ("tool_calling", TC_TEXT, {}),
        "tc_missing_arg": ("tool_calling", TC_MISSING_ARG, {}),
        "tc_wrong_type": ("tool_calling", TC_WRONG_TYPE, {}),
        "tc_bad_json": ("tool_calling", TC_BAD_JSON, {}),
        "tc_final_answer_check": ("tool_calling", TC_PREMATURE_FINAL, {"final_answer_checks": [charge_was_observed]}),
        "tc_max_steps": ("tool_calling", TC_NEVER_FINISHES, {"max_steps": 2}),
        "code_3step": ("code", CA_3STEP, {}),
        "code_1blob": ("code", CA_1BLOB, {}),
        "code_missing_arg": ("code", CA_MISSING_ARG, {}),
        "code_wrong_type": ("code", CA_WRONG_TYPE, {}),
        "code_partial_effect_retry": ("code", CA_PARTIAL_EFFECT, {}),
        "code_no_code_block": ("code", CA_NO_CODE, {}),
    }
    out = {name: run_case(name, *spec[:2], **spec[2]) for name, spec in cases.items()}
    (LOGS / "e1_cases.json").write_text(json.dumps(out, indent=2, default=str))
    return out


# --------------------------------------------------------------------------- E2


def _state_dump_callback(state_path):
    """User-built D1 only: smolagents has no store, so a step callback dumps memory to a file (fsync)."""

    def cb(step, agent=None):
        # NB: callbacks run in _finalize_step, *before* the step is appended to agent.memory.steps,
        # so the current step must be added explicitly or the dump lags one step behind.
        cur = {k: v for k, v in step.dict().items() if k != "model_input_messages"}
        data = json.dumps(agent.memory.get_succinct_steps() + [cur], default=str)
        tmp = str(state_path) + ".tmp"
        with open(tmp, "w") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, state_path)
        if KILL_AT == "W3" and isinstance(step, ActionStep) and any(t.name == "charge" for t in (step.tool_calls or [])):
            os.kill(os.getpid(), signal.SIGKILL)  # after charge returned and its result was recorded (in memory + dump)

    return cb


def build_manager(step_callbacks=None):
    spec_model = ScriptedModel(list(TC_NATIVE))
    specialist = ToolCallingAgent(
        tools=[LookupTool(), ChargeTool()], model=spec_model, name="specialist",
        description="Charges accounts.", verbosity_level=LogLevel.OFF,
    )
    mgr_model = ScriptedModel([
        tc("specialist", {"task": TASK}, "m1"),
        tc("final_answer", {"answer": "done"}, "m2"),
    ])
    manager = ToolCallingAgent(tools=[], model=mgr_model, managed_agents=[specialist], verbosity_level=LogLevel.OFF,
                               step_callbacks=step_callbacks)
    return manager, (mgr_model, spec_model)


def e2_child(window, ledger, state_path):
    global LEDGER, KILL_AT
    LEDGER, KILL_AT = Path(ledger), window
    if window == "W5":
        manager, _ = build_manager(step_callbacks=[_state_dump_callback(state_path)])
        for ev in manager.run(TASK, stream=True):
            if isinstance(ev, ToolCall) and ev.name == "specialist":
                os.kill(os.getpid(), signal.SIGKILL)  # during the delegation boundary
        return
    agent, _ = make_agent("tool_calling", TC_NATIVE, step_callbacks=[_state_dump_callback(state_path)])
    for ev in agent.run(TASK, stream=True):
        if window == "W1" and isinstance(ev, ToolCall) and ev.name == "charge":
            os.kill(os.getpid(), signal.SIGKILL)  # model chose charge; tool not yet invoked


def e2_rerun(window, ledger, state_path):
    """Fresh process. smolagents has no resume API and no way to load memory steps back (no
    ActionStep.from_dict), so the only built-in option is to run the task again from scratch."""
    global LEDGER, KILL_AT
    LEDGER, KILL_AT = Path(ledger), None
    if window == "W5":
        agent, (model, _) = build_manager()
    else:
        agent, model = make_agent("tool_calling", TC_NATIVE)
    res = agent.run(TASK, return_full_result=True)
    print(json.dumps({"output": str(res.output), "state": res.state, "model_calls": model.calls}))


def e2():
    py = sys.executable
    windows = {}
    for w in ["W1", "W2", "W3", "W5"]:
        ledger = LOGS / f"e2_{w}.ledger.jsonl"
        state = LOGS / f"e2_{w}.user_dump.json"
        ledger.unlink(missing_ok=True)
        state.unlink(missing_ok=True)
        p = subprocess.run([py, str(Path(__file__)), "e2-child", w, str(ledger), str(state)], capture_output=True, text=True)
        killed = p.returncode == -signal.SIGKILL
        before = read_ledger(ledger)
        dumped_steps = len(json.loads(state.read_text())) if state.exists() else 0
        r = subprocess.run([py, str(Path(__file__)), "e2-rerun", w, str(ledger), str(state)], capture_output=True, text=True)
        after = read_ledger(ledger)
        charges = [x for x in after if x["tool"] == "charge"]
        lookups = [x for x in after if x["tool"] == "lookup"]
        progress_lost = len(before) > 0  # anything done before the kill was redone by the rerun
        if len(charges) > 1:
            outcome = "duplicate_effect"
        elif len(charges) == 1 and progress_lost:
            outcome = "lost_progress"
        elif len(charges) == 1:
            outcome = "correct"
        else:
            outcome = "stuck"
        windows[w] = {
            "killed_by_sigkill": killed,
            "resumed": False,
            "charges_in_ledger": len(charges),
            "lookups_in_ledger": len(lookups),
            "ledger_lines_before_kill": len(before),
            "user_dump_steps_saved": dumped_steps,
            # the rerun's fresh in-memory state reports one charge; the ledger is ground truth
            "final_state_consistent": len(charges) == 1,
            "progress_lost": progress_lost,
            "manual_intervention": "none possible via API; operator must decide whether to re-run the task from scratch",
            "outcome": outcome,
            "rerun_stdout": r.stdout.strip()[-300:],
            "rerun_rc": r.returncode,
        }
    windows["W4"] = {"outcome": "not_applicable", "notes": "smolagents performs no state write of its own; no hook exists"}
    windows["W6"] = {"outcome": "not_applicable", "notes": "no human-approval suspension primitive"}
    out = {"store": None, "store_note": "no built-in persistence; user step-callback JSON dump used for D1 only", "windows": windows}
    (LOGS / "e2_windows.json").write_text(json.dumps(out, indent=2))
    return out


# --------------------------------------------------------------------------- E3


def e3():
    global LEDGER, KILL_AT
    KILL_AT = None
    LEDGER = LOGS / "e3.ledger.jsonl"
    LEDGER.unlink(missing_ok=True)
    seen_by_specialist = []

    class SpyModel(ScriptedModel):
        def generate(self, messages, **kw):
            seen_by_specialist.append([{"role": str(m.role), "text": last_text([m])[:600]} for m in messages])
            return super().generate(messages, **kw)

    spec_model = SpyModel(list(TC_NATIVE))
    specialist = ToolCallingAgent(tools=[LookupTool(), ChargeTool()], model=spec_model, name="specialist",
                                  description="Charges accounts.", verbosity_level=LogLevel.OFF)
    mgr_model = ScriptedModel([
        tc("specialist", {"task": "Charge acct-7 the looked-up price."}, "m1"),
        tc("final_answer", {"answer": "delegated"}, "m2"),
    ])
    manager = ToolCallingAgent(tools=[], model=mgr_model, managed_agents=[specialist], verbosity_level=LogLevel.OFF)
    res = manager.run("Triage: get acct-7 charged.", return_full_result=True)
    mgr_obs = [s.observations for s in manager.memory.steps if isinstance(s, ActionStep)]
    delegation = {
        "manager_model_calls": mgr_model.calls,
        "specialist_model_calls": spec_model.calls,
        "charges": len([x for x in read_ledger(LEDGER) if x["tool"] == "charge"]),
        "specialist_first_input_messages": seen_by_specialist[0] if seen_by_specialist else None,
        "manager_observation_of_specialist": mgr_obs[0][:800] if mgr_obs else None,
        "specialist_memory_steps_after": len(specialist.memory.steps),
        "manager_state": res.state,
    }

    # Specialist failure: its model raises -> what does the manager see?
    class Boom(Model):
        def generate(self, *a, **k):
            raise RuntimeError("specialist backend down")

    bad = ToolCallingAgent(tools=[ChargeTool()], model=Boom(model_id="boom"), name="specialist",
                           description="Charges accounts.", verbosity_level=LogLevel.OFF)
    mgr2 = ScriptedModel([tc("specialist", {"task": "charge"}, "m1"), tc("final_answer", {"answer": "gave up"}, "m2")])
    manager2 = ToolCallingAgent(tools=[], model=mgr2, managed_agents=[bad], verbosity_level=LogLevel.OFF)
    r2 = manager2.run("Triage", return_full_result=True)
    fail = {
        "manager_continued": r2.state,
        "manager_saw": mgr2.seen[1][:600] if len(mgr2.seen) > 1 else None,
    }

    # Approval probe 1: can a step callback veto charge before it executes? (callbacks run in _finalize_step)
    LEDGER = LOGS / "e3_veto.ledger.jsonl"
    LEDGER.unlink(missing_ok=True)
    vetoed = {"tried": False}

    def veto(step, agent=None):
        if isinstance(step, ActionStep) and any(t.name == "charge" for t in (step.tool_calls or [])):
            vetoed["tried"] = True
            agent.interrupt()

    agent, model = make_agent("tool_calling", TC_NATIVE, step_callbacks=[veto])
    interrupt_err = None
    try:
        agent.run(TASK)
    except Exception as e:  # noqa: BLE001
        interrupt_err = f"{type(e).__name__}: {e}"
    veto_res = {
        "callback_fired_for_charge_step": vetoed["tried"],
        "charges_in_ledger_despite_veto": len([x for x in read_ledger(LEDGER) if x["tool"] == "charge"]),
        "interrupt_result": interrupt_err,
        "model_calls": model.calls,
    }

    # Approval probe 2: stream=True yields ToolCall before execution; can the caller stop it there?
    LEDGER = LOGS / "e3_stream_gate.ledger.jsonl"
    LEDGER.unlink(missing_ok=True)
    agent, model = make_agent("tool_calling", TC_NATIVE)
    gen = agent.run(TASK, stream=True)
    stopped_at = None
    close_error = None
    for ev in gen:
        if isinstance(ev, ToolCall) and ev.name == "charge":
            stopped_at = {"name": ev.name, "arguments": ev.arguments, "id": ev.id}
            try:
                gen.close()  # caller declines; GeneratorExit raised inside the agent's step
            except Exception as e:  # noqa: BLE001
                close_error = f"{type(e).__name__}: {e}"
            break
    stream_gate = {
        "stopped_before": stopped_at,
        "generator_close_error": close_error,
        "charges_in_ledger": len([x for x in read_ledger(LEDGER) if x["tool"] == "charge"]),
        "can_resume_same_run_after_approval": False,
        "notes": "the pending call lives only in the suspended generator frame; closing it ends the run and nothing is serializable",
    }
    out = {
        "supported": False,
        "delegation": delegation,
        "specialist_failure": fail,
        "approval_via_step_callback": veto_res,
        "approval_via_stream_gate": stream_gate,
        "approval_survives_restart": False,
    }
    (LOGS / "e3.json").write_text(json.dumps(out, indent=2, default=str))
    return out


# --------------------------------------------------------------------------- E5


def pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def e5(n=50):
    global LEDGER, KILL_AT
    KILL_AT = None
    res = {"repetitions": n}
    for kind, script in [("tool_calling", TC_NATIVE), ("code", CA_3STEP), ("code_1blob", CA_1BLOB)]:
        LEDGER = LOGS / f"e5_{kind}.ledger.jsonl"
        times = []
        for i in range(n + 1):
            LEDGER.unlink(missing_ok=True)
            t0 = time.perf_counter()
            agent, _ = make_agent("tool_calling" if kind == "tool_calling" else "code", script)
            agent.run(TASK)
            dt = (time.perf_counter() - t0) * 1000
            assert len(read_ledger(LEDGER)) == 2
            if i > 0:
                times.append(dt)
        LEDGER.unlink(missing_ok=True)
        res[f"in_memory_ms_{kind}"] = {
            "p10": round(pct(times, 0.1), 3), "median": round(statistics.median(times), 3), "p90": round(pct(times, 0.9), 3)
        }
    imp = []
    for _ in range(5):
        p = subprocess.run([sys.executable, "-c", "import time;t=time.perf_counter();import smolagents;print((time.perf_counter()-t)*1000)"],
                           capture_output=True, text=True)
        imp.append(float(p.stdout.strip()))
    res["in_memory_ms"] = res["in_memory_ms_tool_calling"]
    res["durable_ms"] = None
    res["import_ms"] = {"runs": [round(x, 1) for x in imp], "median": round(statistics.median(imp), 1)}
    res["notes"] = "per run = agent construction + run; includes 2 ledger fsyncs per run; no durable store exists"
    return res


# --------------------------------------------------------------------------- main


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "all"
    if cmd == "e2-child":
        return e2_child(sys.argv[2], sys.argv[3], sys.argv[4])
    if cmd == "e2-rerun":
        return e2_rerun(sys.argv[2], sys.argv[3], sys.argv[4])
    if cmd == "e1":
        print(json.dumps(e1(), indent=2, default=str))
        return
    if cmd == "e2":
        print(json.dumps(e2(), indent=2))
        return
    if cmd == "e3":
        print(json.dumps(e3(), indent=2, default=str))
        return
    if cmd == "e5":
        print(json.dumps(e5(), indent=2))
        return
    e1r = e1()
    e2r = e2()
    e3r = e3()
    e5r = e5()
    nat = e1r["tc_native"]
    results = {
        "candidate": "smolagents",
        "commit": COMMIT,
        "package": f"smolagents=={smolagents.__version__}",
        "environment": {"cpus": os.cpu_count(), "python": platform.python_version(), "os": sys.platform,
                        "machine": platform.machine(), "platform": platform.platform()},
        "e1": {
            "success": nat["verified"],
            "model_calls": nat["model_calls"],
            "tool_calls": nat["tool_calls_in_ledger"],
            "invalid_args_handled": "error-as-observation: AgentToolCallError text appended to memory as a tool-response "
                                    "message ending 'Now let's retry...'; next model call recovers; no framework retry",
            "retries": 0,
            "tokens": None,
            "wall_ms": nat["wall_ms"],
            "variants": {k: {kk: v[kk] for kk in ("agent", "verified", "model_calls", "tool_calls_in_ledger", "charges",
                                                   "error_steps", "run_state", "raised", "wall_ms")}
                         for k, v in e1r.items()},
        },
        "e2": e2r,
        "e3": {"supported": False, "notes": "managed agents = synchronous agent-as-tool delegation; no approval, no "
                                             "suspension, no restart survival", **e3r},
        "e5": e5r,
        "blocked": ["E4: no local inference endpoint reachable in this environment"],
    }
    (HERE / "results.json").write_text(json.dumps(results, indent=2, default=str))
    print(json.dumps(results["e1"], indent=2, default=str))


if __name__ == "__main__":
    main()
