"""OpenHands software-agent-sdk harness for the agent-runtime-primitives protocol.

Scripted fake LLM (a subclass of the SDK's own ``openhands.sdk.testing.TestLLM``),
fake ``lookup``/``charge`` tools that append to an fsynced ``ledger.jsonl`` outside the
conversation store, and deterministic SIGKILL points. No network model is called.

Usage (each window in a fresh, empty work dir; always run with ``python -I``):

    python -I harness.py run    <workdir> <window> <path>   # path: sdk | server
    python -I harness.py resume <workdir> <window> <path> [--script retry-on-error]
    python -I harness.py e1     <workdir> [invalid]
    python -I harness.py e5     <workdir> <reps>

Windows: none, W1, W2, W3, W4a, W4b, W6 (W6 uses confirmation mode).
``sdk`` drives ``LocalConversation`` directly (resume = construct it again with the same
``persistence_dir`` + ``conversation_id`` and call ``run()``, as the SDK's
``examples/01_standalone_sdk/10_persistence.py`` does). ``server`` drives the agent
server's ``EventService`` in-process (its ``start()`` holds the lease claim and the
stale-RUNNING recovery rule), without HTTP.
"""

from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import time
import uuid
from collections.abc import Sequence
from contextlib import contextmanager
from pathlib import Path
from typing import Any

os.environ.setdefault("OPENHANDS_SUPPRESS_BANNER", "1")
os.environ.setdefault("LITELLM_LOCAL_MODEL_COST_MAP", "True")

T_IMPORT0 = time.perf_counter()
from pydantic import Field  # noqa: E402

from openhands.sdk import (  # noqa: E402
    Action,
    Agent,
    Conversation,
    Observation,
    TextContent,
    Tool,
    ToolDefinition,
)
from openhands.sdk.conversation.state import (  # noqa: E402
    ConversationExecutionStatus,
    ConversationState,
)
from openhands.sdk.event import (  # noqa: E402
    ActionEvent,
    AgentErrorEvent,
    ObservationEvent,
    UserRejectObservation,
)
from openhands.sdk.llm import Message, MessageToolCall  # noqa: E402
from openhands.sdk.security.confirmation_policy import AlwaysConfirm  # noqa: E402
from openhands.sdk.testing import TestLLM  # noqa: E402
from openhands.sdk.tool import ToolExecutor, register_tool  # noqa: E402

T_IMPORT = time.perf_counter() - T_IMPORT0

CONV_ID = uuid.UUID("00000000-0000-4000-8000-0000000000e2")
PRICE = 42

# ---------------------------------------------------------------- run context
CTX: dict[str, Any] = {
    "workdir": None,
    "window": "none",
    "phase": "run",
    "script": "protocol",
    "charge_returned": False,
    "guard_fired": False,
    "model_calls": 0,
}


def ledger_path() -> Path:
    return Path(CTX["workdir"]) / "ledger.jsonl"


def trace(**kw: Any) -> None:
    kw.setdefault("pid", os.getpid())
    kw.setdefault("phase", CTX["phase"])
    with open(Path(CTX["workdir"]) / "trace.jsonl", "a") as f:
        f.write(json.dumps(kw) + "\n")
        f.flush()
        os.fsync(f.fileno())


def ledger_append(tool: str, args: dict[str, Any]) -> int:
    path = ledger_path()
    n = sum(1 for _ in open(path)) if path.exists() else 0
    line = {"tool": tool, "args": args, "invocation": n + 1, "pid": os.getpid(), "t": time.time()}
    with open(path, "a") as f:
        f.write(json.dumps(line) + "\n")
        f.flush()
        os.fsync(f.fileno())
    return n + 1


def die(point: str) -> None:
    trace(event="sigkill", point=point)
    os.kill(os.getpid(), signal.SIGKILL)


# ---------------------------------------------------------------------- tools
class LookupAction(Action):
    key: str = Field(description="The key to look up")


class ChargeAction(Action):
    account: str = Field(description="Account to charge")
    amount: int = Field(description="Amount to charge")


class LookupObservation(Observation):
    pass


class ChargeObservation(Observation):
    pass


class LookupExecutor(ToolExecutor[LookupAction, LookupObservation]):
    def __call__(self, action: LookupAction, conversation=None) -> Observation:  # noqa: ARG002
        ledger_append("lookup", {"key": action.key})
        return LookupObservation.from_text(str(PRICE))


class ChargeExecutor(ToolExecutor[ChargeAction, ChargeObservation]):
    def __call__(self, action: ChargeAction, conversation=None) -> Observation:  # noqa: ARG002
        if CTX["phase"] == "run" and CTX["window"] == "W1":
            die("W1: charge entry, before ledger write")
        # The executor receives only the Action: there is no tool_call_id or
        # idempotency key in its signature to forward.
        inv = ledger_append("charge", {"account": action.account, "amount": action.amount})
        if CTX["phase"] == "run" and CTX["window"] in ("W2", "W2c"):
            die(f"{CTX['window']}: inside charge, after ledger fsync")
        CTX["charge_returned"] = True
        return ChargeObservation.from_text(f"charged {action.account} {action.amount} (ledger #{inv})")


class LookupTool(ToolDefinition[LookupAction, LookupObservation]):
    @classmethod
    def create(cls, conv_state=None, **params) -> Sequence[ToolDefinition]:  # noqa: ARG003
        return [
            cls(
                description="Look up a value by key.",
                action_type=LookupAction,
                observation_type=LookupObservation,
                executor=LookupExecutor(),
            )
        ]


class ChargeTool(ToolDefinition[ChargeAction, ChargeObservation]):
    @classmethod
    def create(cls, conv_state=None, **params) -> Sequence[ToolDefinition]:  # noqa: ARG003
        return [
            cls(
                description="Charge an account. Consequential.",
                action_type=ChargeAction,
                observation_type=ChargeObservation,
                executor=ChargeExecutor(),
            )
        ]


register_tool("lookup", LookupTool)
register_tool("charge", ChargeTool)


# ------------------------------------------------------------- scripted model
def _text(m: Message) -> str:
    return "".join(c.text for c in m.content if isinstance(c, TextContent))


def recorded_replies(conv_path: Path) -> int:
    """Agent replies already recorded in the persisted log (full log, not the branch):
    distinct ``llm_response_id`` of agent ActionEvents and agent MessageEvents."""
    ids = set()
    events_dir = conv_path / "events"
    for p in sorted(events_dir.glob("*.json")) if events_dir.exists() else []:
        d = json.loads(p.read_text())
        if d.get("source") == "agent" and d.get("kind") in ("ActionEvent", "MessageEvent"):
            ids.add(d.get("llm_response_id") or d.get("id"))
    return len(ids)


class ScriptedLLM(TestLLM):
    """Scripted fake model. Two index rules (``CTX['index_rule']``):

      count    (the protocol fixture) index = model calls in this process + agent replies
               already recorded in the persisted log at process start.
      visible  the reply depends on the tool results the model is actually shown: no
               lookup result -> lookup; no charge result -> charge; else final. This is
               what a competent real model does with the history it is given.

    Scripts: protocol (lookup -> charge -> final); invalid (lookup -> charge without
    amount -> charge -> final; count rule only); retry-on-error (like protocol, but a
    charge result that is an error makes it issue charge again; a sensitivity variant).
    """

    def completion(self, messages, tools=None, add_security_risk_prediction=False,  # noqa: ARG002
                   on_token=None, call_context=None, **kwargs):  # noqa: ARG002
        count_idx = CTX.get("recorded_at_start", 0) + CTX["model_calls"]
        CTX["model_calls"] += 1
        tool_msgs = {m.tool_call_id: _text(m) for m in messages if m.role == "tool"}
        n_assistant = sum(1 for m in messages if m.role == "assistant")
        trace(event="model_call", count_index=count_idx, visible_assistant=n_assistant,
              n_messages=len(messages), tool_results=tool_msgs)
        if CTX["phase"] == "run" and CTX["window"] == "W3" and count_idx == 2:
            die("W3: start of model call 3 (charge result recorded)")
        script = CTX["script"]
        charge_results = [v for k, v in tool_msgs.items() if k and k.startswith("call_charge")]
        if CTX.get("index_rule", "count") == "visible":
            if "call_lookup_1" not in tool_msgs:
                step = "lookup"
            elif not charge_results:
                step = "charge"
            else:
                step = "final"
        else:
            plan = (["lookup", "charge_bad", "charge", "final"] if script == "invalid"
                    else ["lookup", "charge", "final"])
            step = plan[min(count_idx, len(plan) - 1)]
        if script == "retry-on-error" and step == "final":
            if charge_results and not charge_results[-1].startswith("charged"):
                step = "charge_retry"
        trace(event="model_reply", step=step, rule=CTX.get("index_rule", "count"))
        self._scripted_responses.append(self._reply(step, tool_msgs))
        return super().completion(messages, tools=tools, **kwargs)

    @staticmethod
    def _reply(step: str, tool_msgs: dict[str | None, str]) -> Message:
        def call(cid: str, name: str, args: dict[str, Any]) -> Message:
            return Message(role="assistant", content=[TextContent(text="")],
                           tool_calls=[MessageToolCall(id=cid, name=name,
                                                       arguments=json.dumps(args),
                                                       origin="completion")])
        raw = tool_msgs.get("call_lookup_1", str(PRICE)).strip()
        value = int(raw) if raw.isdigit() else PRICE
        if step == "lookup":
            return call("call_lookup_1", "lookup", {"key": "price"})
        if step == "charge":
            return call("call_charge_1", "charge", {"account": "acct-7", "amount": value})
        if step == "charge_bad":
            return call("call_charge_bad", "charge", {"account": "acct-7"})
        if step == "charge_retry":
            return call("call_charge_2", "charge", {"account": "acct-7", "amount": value})
        return Message(role="assistant",
                       content=[TextContent(text=f"Charged acct-7 the amount {value}.")])


# The agent server re-validates the agent (``model_validate(model_dump())``) on start and
# loads it from base_state.json on resume, which turns ScriptedLLM back into a plain LLM.
# Route every LLM instance's completion to the script so no provider is ever called.
from openhands.sdk.llm.llm import LLM as _LLM  # noqa: E402

_RESPONDER: ScriptedLLM | None = None


def _responder() -> ScriptedLLM:
    global _RESPONDER
    if _RESPONDER is None:
        _RESPONDER = ScriptedLLM(model="test-model", usage_id="scripted")
    return _RESPONDER


def _patched_completion(self, messages, *args, **kwargs):  # noqa: ARG001
    return _responder().completion(messages, *args, **kwargs)


async def _patched_acompletion(self, messages, *args, **kwargs):  # noqa: ARG001
    return _responder().completion(messages, *args, **kwargs)


_LLM.completion = _patched_completion
_LLM.acompletion = _patched_acompletion
_LLM.uses_responses_api = lambda self: False  # noqa: ARG005


GOAL = ("Charge account acct-7 the amount lookup('price') returns, then report the charge.")


def make_agent() -> Agent:
    return Agent(llm=ScriptedLLM(model="test-model", usage_id="scripted"),
                 tools=[Tool(name="lookup"), Tool(name="charge")])


# ------------------------------------------------------- W4 write-guard hook
@contextmanager
def _guard_cm(kind: str):
    w = CTX["window"]
    armed = CTX["phase"] == "run" and CTX["charge_returned"] and not CTX["guard_fired"] \
        and kind == "append"
    if armed and w == "W4a":
        CTX["guard_fired"] = True
        die("W4a: EventLog.append write guard entered for the charge observation, before file write")
    yield
    if armed and w == "W4b":
        CTX["guard_fired"] = True
        die("W4b: charge observation file written, before index/length-marker/HEAD advance")


def write_guard():
    # Which persistence call is this? EventLog.append or ConversationState._save_base_state.
    caller = sys._getframe(1).f_code.co_name
    return _guard_cm(caller)


# ------------------------------------------------------------------- reading
def persisted_summary(conv_dir: Path) -> dict[str, Any]:
    events_dir = conv_dir / "events"
    files = sorted(p.name for p in events_dir.glob("*.json")) if events_dir.exists() else []
    kinds = []
    for name in files:
        try:
            d = json.loads((events_dir / name).read_text())
            kinds.append(d.get("kind"))
        except Exception as e:  # noqa: BLE001
            kinds.append(f"unreadable:{e}")
    status = None
    lease = None
    bs = conv_dir / "base_state.json"
    if bs.exists():
        status = json.loads(bs.read_text()).get("execution_status")
    lf = conv_dir / "owner_lease.json"
    if lf.exists():
        lease = json.loads(lf.read_text())
    return {"event_files": len(files), "kinds": kinds, "base_state_status": status,
            "lease": lease}


def ledger_counts(workdir: Path) -> dict[str, Any]:
    p = workdir / "ledger.jsonl"
    lines = [json.loads(x) for x in open(p)] if p.exists() else []
    return {"lookup": sum(1 for x in lines if x["tool"] == "lookup"),
            "charge": sum(1 for x in lines if x["tool"] == "charge"),
            "charge_pids": [x["pid"] for x in lines if x["tool"] == "charge"],
            "charge_amounts": [x["args"].get("amount") for x in lines if x["tool"] == "charge"]}


def state_summary(state: ConversationState) -> dict[str, Any]:
    evs = list(state.events)
    obs_charge = [e for e in evs if isinstance(e, ObservationEvent) and e.tool_name == "charge"]
    err_charge = [e for e in evs if isinstance(e, AgentErrorEvent) and e.tool_name == "charge"]
    rej = [e for e in evs if isinstance(e, UserRejectObservation)]
    act_charge = [e for e in evs if isinstance(e, ActionEvent) and e.tool_name == "charge"]
    return {
        "status": str(state.execution_status.value),
        "n_events": len(evs),
        "charge_actions": len(act_charge),
        "charge_observations": len(obs_charge),
        "charge_errors": [e.error[:120] for e in err_charge],
        "rejections": len(rej),
        "unmatched_active_branch": [e.tool_name for e in
                                    ConversationState.get_unmatched_actions(state.active_branch())],
        "unmatched_full_log": [e.tool_name for e in ConversationState.get_unmatched_actions(evs)],
    }


# -------------------------------------------------------------- SDK driving
def sdk_conversation(workdir: Path, agent: Agent | None):
    conv = Conversation(agent=agent, workspace=str(workdir / "ws"),
                        persistence_dir=str(workdir / "conv"), conversation_id=CONV_ID,
                        visualizer=None, delete_on_close=False)
    conv.state.set_write_guard(write_guard)
    return conv


def conv_dir(workdir: Path) -> Path:
    return workdir / "conv" / CONV_ID.hex


def cmd_run_sdk(workdir: Path, window: str) -> None:
    (workdir / "ws").mkdir(parents=True, exist_ok=True)
    conv = sdk_conversation(workdir, make_agent())
    if window in ("W6", "W2c"):
        conv.set_confirmation_policy(AlwaysConfirm())
    conv.send_message(GOAL)
    for _ in range(10):
        conv.run()
        st = conv.state
        if st.execution_status == ConversationExecutionStatus.WAITING_FOR_CONFIRMATION:
            pending = ConversationState.get_unmatched_actions(st.active_branch())
            names = [p.tool_name for p in pending]
            trace(event="waiting_for_confirmation", pending=names)
            if "charge" in names and window == "W6":
                die("W6: suspended awaiting approval of charge")
            continue  # run() again = implicit approval of the pending lookup
        break
    trace(event="run_finished", state=state_summary(conv.state))


def cmd_resume_sdk(workdir: Path) -> dict[str, Any]:
    before = persisted_summary(conv_dir(workdir))
    conv = sdk_conversation(workdir, make_agent())
    loaded = state_summary(conv.state)
    trace(event="resume_loaded", state=loaded)
    t0 = time.perf_counter()
    approvals = 0
    error = None
    if CTX.get("reject"):
        conv.reject_pending_actions("rejected by a person after the restart")
    try:
        for _ in range(10):
            conv.run()
            if conv.state.execution_status == ConversationExecutionStatus.WAITING_FOR_CONFIRMATION:
                approvals += 1  # approve = run() again (the SDK's documented confirmation)
                continue
            break
    except Exception as e:  # noqa: BLE001
        error = f"{type(e).__name__}: {e}"
    return {"persisted_before_resume": before, "loaded": loaded,
            "after": state_summary(conv.state), "approvals_by_run": approvals,
            "resume_error": error, "resume_ms": round((time.perf_counter() - t0) * 1000)}


# ----------------------------------------------------------- server driving
def _server_imports():
    from openhands.agent_server.event_service import EventService
    from openhands.agent_server.models import StoredConversation
    from openhands.sdk.workspace import LocalWorkspace
    return EventService, StoredConversation, LocalWorkspace


def _stored(workdir: Path, window: str):
    _, StoredConversation, LocalWorkspace = _server_imports()
    kw: dict[str, Any] = {}
    if window in ("W6", "W2c"):
        kw["confirmation_policy"] = AlwaysConfirm()
    return StoredConversation(id=CONV_ID, workspace=LocalWorkspace(working_dir=str(workdir / "ws")),
                              **kw)


async def _server_run(workdir: Path, window: str) -> None:
    EventService, _, _ = _server_imports()
    (workdir / "ws").mkdir(parents=True, exist_ok=True)
    svc = EventService(stored=_stored(workdir, window), conversations_dir=workdir / "conv",
                       agent=make_agent())
    await svc.start()
    svc._conversation.state.set_write_guard(_chain_guard(svc))  # noqa: SLF001
    trace(event="server_started", lease=persisted_summary(conv_dir(workdir))["lease"])
    await svc.send_message(Message(role="user", content=[TextContent(text=GOAL)]), run=True)
    for _ in range(10):
        status = await svc.wait_for_run_completion(timeout=60)
        st = svc._conversation.state  # noqa: SLF001
        if status == ConversationExecutionStatus.WAITING_FOR_CONFIRMATION:
            names = [p.tool_name for p in ConversationState.get_unmatched_actions(st.events)]
            trace(event="waiting_for_confirmation", pending=names)
            if "charge" in names and window == "W6":
                die("W6: suspended awaiting approval of charge (server)")
            await svc.run()  # what respond_to_confirmation(accept=True) does
            continue
        break
    trace(event="run_finished", state=state_summary(svc._conversation.state))  # noqa: SLF001
    await svc.close()


def _chain_guard(svc):
    """Compose the harness kill guard with the server's lease guard (both run)."""
    lease_guard = svc._write_guard  # noqa: SLF001

    @contextmanager
    def cm(kind):
        with lease_guard():
            with _guard_cm(kind):
                yield

    def factory():
        return cm(sys._getframe(1).f_code.co_name)
    return factory


async def _server_resume(workdir: Path, window: str) -> dict[str, Any]:
    EventService, _, _ = _server_imports()
    before = persisted_summary(conv_dir(workdir))
    svc = EventService(stored=_stored(workdir, window), conversations_dir=workdir / "conv",
                       agent=None)
    t0 = time.perf_counter()
    error = None
    approvals = 0
    try:
        await svc.start()
        loaded = state_summary(svc._conversation.state)  # noqa: SLF001
        lease_after_start = persisted_summary(conv_dir(workdir))["lease"]
        trace(event="resume_loaded", state=loaded)
        await svc.run()
        for _ in range(10):
            status = await svc.wait_for_run_completion(timeout=60)
            if status == ConversationExecutionStatus.WAITING_FOR_CONFIRMATION:
                approvals += 1
                await svc.run()
                continue
            break
        after = state_summary(svc._conversation.state)  # noqa: SLF001
        await svc.close()
    except Exception as e:  # noqa: BLE001
        error = f"{type(e).__name__}: {e}"
        loaded, after, lease_after_start = None, None, None
    return {"persisted_before_resume": before, "loaded_after_recovery_rule": loaded,
            "lease_after_start": lease_after_start, "after": after,
            "approvals_by_run": approvals, "resume_error": error,
            "resume_ms": round((time.perf_counter() - t0) * 1000)}


# ------------------------------------------------------------------- E1 / E5
def cmd_e1(workdir: Path, invalid: bool) -> dict[str, Any]:
    (workdir / "ws").mkdir(parents=True, exist_ok=True)
    CTX["script"] = "invalid" if invalid else "protocol"
    t0 = time.perf_counter()
    conv = sdk_conversation(workdir, make_agent())
    conv.send_message(GOAL)
    conv.run()
    wall = (time.perf_counter() - t0) * 1000
    st = state_summary(conv.state)
    evs = list(conv.state.events)
    errs = [e.error[:200] for e in evs if isinstance(e, AgentErrorEvent)]
    final = [e for e in evs if type(e).__name__ == "MessageEvent" and e.source == "agent"]
    return {"wall_ms": round(wall), "model_calls": CTX["model_calls"], "state": st,
            "agent_errors": errs, "ledger": ledger_counts(workdir),
            "final_text": _text(final[-1].llm_message) if final else None}


def cmd_e5(workdir: Path, reps: int) -> dict[str, Any]:
    import statistics

    def one(i: int, durable: bool) -> float:
        d = workdir / f"r{i}{'d' if durable else 'm'}"
        (d / "ws").mkdir(parents=True, exist_ok=True)
        CTX["workdir"] = str(d)
        CTX["model_calls"] = 0
        t0 = time.perf_counter()
        conv = Conversation(agent=make_agent(), workspace=str(d / "ws"),
                            persistence_dir=str(d / "conv") if durable else None,
                            conversation_id=uuid.uuid4(), visualizer=None, delete_on_close=False)
        conv.send_message(GOAL)
        conv.run()
        ms = (time.perf_counter() - t0) * 1000
        lc = ledger_counts(d)
        assert lc["charge"] == 1 and lc["lookup"] == 1, lc
        return ms

    out: dict[str, Any] = {"repetitions": reps}
    for durable in (False, True):
        one(-1, durable)  # warm-up
        xs = sorted(one(i, durable) for i in range(reps))
        q = statistics.quantiles(xs, n=10)
        key = "durable_ms" if durable else "in_memory_ms"
        out[key] = {"p10": round(q[0], 1), "median": round(statistics.median(xs), 1),
                    "p90": round(q[-1], 1)}
    return out


def cmd_lease(workdir: Path) -> dict[str, Any]:
    """The agent server's ConversationLease directly: live-owner refusal, expiry takeover
    with generation + 1, and fencing of the old owner's writes."""
    from openhands.agent_server.conversation_lease import (
        ConversationLease, ConversationLeaseHeldError, ConversationOwnershipLostError)
    d = workdir / "leasedir"
    a = ConversationLease(conversation_dir=d, owner_instance_id="owner-a", ttl_seconds=1.0)
    b = ConversationLease(conversation_dir=d, owner_instance_id="owner-b", ttl_seconds=1.0)
    out: dict[str, Any] = {}
    ca = a.claim()
    out["a_claim"] = {"generation": ca.generation, "takeover": ca.takeover}
    try:
        b.claim()
        out["b_claim_while_a_live"] = "granted"
    except ConversationLeaseHeldError as e:
        out["b_claim_while_a_live"] = f"refused: {type(e).__name__}"
    time.sleep(1.3)  # let A's lease expire (A's pid is alive: only TTL expiry applies)
    cb = b.claim()
    out["b_claim_after_expiry"] = {"generation": cb.generation, "takeover": cb.takeover}
    try:
        with a.guarded_write(ca.generation):
            pass
        out["a_write_after_takeover"] = "allowed"
    except ConversationOwnershipLostError as e:
        out["a_write_after_takeover"] = f"refused: {type(e).__name__}"
    try:
        a.renew(ca.generation)
        out["a_renew_after_takeover"] = "allowed"
    except ConversationOwnershipLostError as e:
        out["a_renew_after_takeover"] = f"refused: {type(e).__name__}"
    (d / "owner_lease.json").write_text("{corrupt")
    cc = a.claim()
    out["claim_after_corrupt_lease_file"] = {"generation": cc.generation, "takeover": cc.takeover}
    return out


# ---------------------------------------------------------------------- main
def main() -> None:
    cmd, workdir = sys.argv[1], Path(sys.argv[2]).resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    CTX["workdir"] = str(workdir)
    if cmd == "run":
        window, path = sys.argv[3], sys.argv[4]
        CTX.update(window=window, phase="run")
        trace(event="start", window=window, path=path, import_s=round(T_IMPORT, 2))
        if path == "sdk":
            cmd_run_sdk(workdir, window)
        else:
            asyncio.run(_server_run(workdir, window))
        print(json.dumps({"exited_normally": True}))
    elif cmd == "resume":
        window, path = sys.argv[3], sys.argv[4]
        CTX["reject"] = "--reject" in sys.argv
        CTX["index_rule"] = "visible" if "--visible" in sys.argv else "count"
        CTX["recorded_at_start"] = recorded_replies(conv_dir(workdir))
        if "--script" in sys.argv:
            CTX["script"] = sys.argv[sys.argv.index("--script") + 1]
        CTX.update(window=window, phase="resume")
        trace(event="start", window=window, path=path)
        if path == "sdk":
            res = cmd_resume_sdk(workdir)
        else:
            res = asyncio.run(_server_resume(workdir, window))
        res["ledger"] = ledger_counts(workdir)
        res["model_calls_in_resume"] = CTX["model_calls"]
        res["index_rule"] = CTX["index_rule"]
        res["recorded_replies_at_start"] = CTX["recorded_at_start"]
        print(json.dumps(res))
    elif cmd == "e1":
        res = cmd_e1(workdir, len(sys.argv) > 3 and sys.argv[3] == "invalid")
        print(json.dumps(res))
    elif cmd == "lease":
        print(json.dumps(cmd_lease(workdir)))
    elif cmd == "e5":
        res = cmd_e5(workdir, int(sys.argv[3]))
        res["import_s"] = round(T_IMPORT, 2)
        print(json.dumps(res))


if __name__ == "__main__":
    main()
