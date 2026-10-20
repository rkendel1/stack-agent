"""E1/E2 harness for CrewAI Flows. Public API only, no LLM (scripted fake model inside flow methods).

  python -I harness.py run|resume <workdir> <window> <mech>
mech: persist  -> class-level @persist(SQLiteFlowPersistence); resume = kickoff(inputs={"id": <flow id>})
      ckpt     -> CheckpointConfig(SqliteProvider, on_events=["method_execution_finished"]);
                  resume = kickoff(from_checkpoint=CheckpointConfig(restore_from=<latest row>))
      approval -> @persist + @human_feedback with a provider that raises HumanFeedbackPending;
                  resume = Flow.from_pending(id).resume("approved")
"""
from __future__ import annotations

import json, os, signal, sqlite3, sys, time
from typing import Any

from pydantic import BaseModel, Field

from crewai.flow.flow import Flow, listen, start
from crewai.flow.persistence import SQLiteFlowPersistence, persist
from crewai.flow.human_feedback import human_feedback
from crewai.flow.async_feedback import HumanFeedbackPending, HumanFeedbackProvider
from crewai.state.checkpoint_config import CheckpointConfig
from crewai.state.provider.sqlite_provider import SqliteProvider

MODE, WORKDIR, WINDOW, MECH = sys.argv[1:5]
LEDGER = os.path.join(WORKDIR, "ledger.jsonl")
DB = os.path.join(WORKDIR, "store", "flow.db")
CKPT_DB = os.path.join(WORKDIR, "store", "ckpt.db")
os.makedirs(os.path.dirname(DB), exist_ok=True)
MODEL_CALLS = 0
CHARGED_THIS_PROCESS = {"v": False}


def die(point):
    open(os.path.join(WORKDIR, "killpoint.txt"), "w").write(point)
    os.kill(os.getpid(), signal.SIGKILL)


def kill_if(point):
    if MODE == "run" and WINDOW == point:
        die(point)


def ledger_append(tool, args, invocation):
    line = json.dumps({"tool": tool, "args": args, "invocation": invocation, "pid": os.getpid(), "t": time.time()})
    fd = os.open(LEDGER, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, (line + "\n").encode()); os.fsync(fd)
    finally:
        os.close(fd)


class S(BaseModel):
    id: str = ""
    history: list[dict] = Field(default_factory=list)


def fake_model(history, step):
    """Scripted fake model keyed by the calling decision step.

    The protocol fixture indexes replies by count of recorded replies; that fixture crashes
    immediately under @persist's rerun-from-@start semantics (state already holds later replies
    when decide_lookup runs again), which hides the effect behaviour. Keying by decision step
    keeps every method deterministic so the ledger shows what re-execution does.
    """
    global MODEL_CALLS
    MODEL_CALLS += 1
    if step == "decide_lookup":
        return {"role": "assistant", "call": {"tool": "lookup", "args": {"key": "price"}}}
    if step == "decide_charge":
        price = [h["result"] for h in history if h.get("tool") == "lookup"][-1]
        return {"role": "assistant", "call": {"tool": "charge", "args": {"account": "acct-7", "amount": price}}}
    amt = [h["result"]["amount"] for h in history if h.get("tool") == "charge"][-1]
    return {"role": "assistant", "final": f"Charged acct-7 {amt}"}


class KillablePersistence(SQLiteFlowPersistence):
    def save_state(self, flow_uuid, method_name, state_data):
        if method_name == "do_charge":
            kill_if("W4")  # about to persist the state that records the charge
        return super().save_state(flow_uuid, method_name, state_data)


class PendingProvider(HumanFeedbackProvider):
    def request_feedback(self, context, flow):
        raise HumanFeedbackPending(context=context)


def _decide_lookup(self):
    self.state.history.append(fake_model(self.state.history, "decide_lookup"))


def _do_lookup(self):
    ledger_append("lookup", {"key": "price"}, "c1")
    self.state.history.append({"role": "tool", "tool": "lookup", "result": 42})


def _decide_charge(self):
    self.state.history.append(fake_model(self.state.history, "decide_charge"))


def _charge(self):
    kill_if("W1")  # model chose charge (persisted), charge not yet invoked
    args = [h["call"]["args"] for h in self.state.history if h.get("call", {}).get("tool") == "charge"][-1]
    ledger_append("charge", args, "c2")
    kill_if("W2")  # effect fsynced in ledger, method has not returned
    self.state.history.append({"role": "tool", "tool": "charge", "result": {"ok": True, "amount": args["amount"]}})


def _report(self):
    kill_if("W3")  # do_charge returned and (for @persist) its state row was committed
    r = fake_model(self.state.history, "report")
    self.state.history.append(r)
    return r["final"]


# Flow methods are declared directly on each class (the Flow metaclass did not pick up
# methods inherited from a base Flow subclass in a first attempt: zero methods ran).
class PlainFlow(Flow[S]):
    @start()
    def decide_lookup(self):
        _decide_lookup(self)

    @listen(decide_lookup)
    def do_lookup(self):
        _do_lookup(self)

    @listen(do_lookup)
    def decide_charge(self):
        _decide_charge(self)

    @listen(decide_charge)
    def do_charge(self):
        _charge(self)

    @listen(do_charge)
    def report(self):
        return _report(self)


class ApprovalFlow(Flow[S]):
    @start()
    def decide_lookup(self):
        _decide_lookup(self)

    @listen(decide_lookup)
    def do_lookup(self):
        _do_lookup(self)

    @listen(do_lookup)
    def decide_charge(self):
        _decide_charge(self)

    @listen(decide_charge)
    @human_feedback(message="Approve charge?", provider=PendingProvider())
    def approve_charge(self):
        return json.dumps(self.state.history[-1]["call"])

    @listen(approve_charge)
    def do_charge(self):
        _charge(self)

    @listen(do_charge)
    def report(self):
        return _report(self)


def latest_ckpt():
    if not os.path.exists(CKPT_DB):
        return None
    with sqlite3.connect(CKPT_DB) as c:
        try:
            row = c.execute("SELECT id FROM checkpoints ORDER BY rowid DESC LIMIT 1").fetchone()
        except sqlite3.OperationalError:  # table never created: no checkpoint landed before the kill
            return None
    return f"{CKPT_DB}#{row[0]}" if row else None


def main():
    out: dict[str, Any] = {"mode": MODE, "mech": MECH}
    t0 = time.perf_counter()
    idf = os.path.join(WORKDIR, "flow_id.txt")
    if MECH in ("persist", "approval"):
        p = KillablePersistence(DB)
        cls = ApprovalFlow if MECH == "approval" else PlainFlow
        cls = persist(p)(cls)
        if MODE == "run":
            f = cls(persistence=p)
            open(idf, "w").write(f.state.id)
            result = f.kickoff()
        elif MECH == "approval":
            fid = open(idf).read()
            try:
                f = cls.from_pending(fid, p)
                out["resumed_via"] = "from_pending"
                result = f.resume("approved")
            except ValueError as e:  # no pending record -> fall back to state reload
                out["from_pending_error"] = str(e)
                f = cls(persistence=p)
                result = f.kickoff(inputs={"id": fid})
                out["resumed_via"] = "kickoff(inputs={id})"
        else:
            fid = open(idf).read()
            f = cls(persistence=p)
            result = f.kickoff(inputs={"id": fid})
            out["resumed_via"] = "kickoff(inputs={id})"
    else:  # ckpt
        cfg = CheckpointConfig(location=CKPT_DB, provider=SqliteProvider(), on_events=["method_execution_finished"])
        if MODE == "run":
            f = PlainFlow(checkpoint=cfg)
            result = f.kickoff()
        else:
            loc = latest_ckpt()
            out["resumed_from"] = loc
            if loc is None:
                out.update({"output": None, "pending": False, "model_calls_this_process": 0,
                            "state_charge_results": None, "error": "no checkpoint row existed after the kill"})
                return out
            f = PlainFlow(checkpoint=cfg)
            result = f.kickoff(from_checkpoint=CheckpointConfig(location=CKPT_DB, provider=SqliteProvider(), restore_from=loc))
            out["resumed_via"] = "kickoff(from_checkpoint=...)"
    if MODE == "run" and WINDOW == "W6" and isinstance(result, HumanFeedbackPending):
        die("W6")
    out["pending"] = isinstance(result, HumanFeedbackPending)
    out["output"] = None if out["pending"] else result
    out["model_calls_this_process"] = MODEL_CALLS
    out["state_charge_results"] = sum(1 for h in f.state.history if h.get("tool") == "charge")
    out["state_lookup_results"] = sum(1 for h in f.state.history if h.get("tool") == "lookup")
    out["wall_ms"] = round((time.perf_counter() - t0) * 1000, 2)
    return out


if __name__ == "__main__":
    res = main()
    json.dump(res, open(os.path.join(WORKDIR, f"{MODE}.json"), "w"), indent=2, default=str)
    print(json.dumps(res, default=str))
