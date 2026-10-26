"""One trial: a task, a configuration, a model, a trial index, through the real CLI.

The harness owns everything around the work: it builds the checkout, seeds any history, starts and
stops processes, enforces the wall-clock limit, injects faults, resumes, settles as an operator when
the runtime refuses to guess, and verifies the result independently. `stack-agent` owns the work.

A trial never reads the evaluator's files into the checkout the agent works in.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from . import RUN_SCHEMA, paxwrap
from .configs import model_flags, upstream_key, work_flags
from .controls import Script, control_script, seed_script
from .proxy import ModelProxy
from .suite import (Task, apply_overlay, cargo_tests, git, materialize, reset_to,
                    split_results)
from .util import iso, now_ms, redact_value, run, sha256_bytes
from .verify import verify

CHILD_SECRET_VARS = ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "BENCH_API_KEY", "AI_GATEWAY_API_KEY",
                     "CHIP_API_KEY", "CHIP_ENDPOINT", "CHIP_MODEL", "CHIP_PROVIDER")
LEASE_SECS = 2
MAX_RESUMES = 3


@dataclass
class Env:
    """Where the binaries are and where trials run."""
    stack_agent: str
    pax: str
    work_root: Path
    identity: dict  # binaries' versions and commits, recorded with every run


def child_env(env: Env, rundir: Path, pax_env: dict) -> dict:
    out = {k: v for k, v in os.environ.items() if k not in CHILD_SECRET_VARS}
    out.update({
        "PAX_BIN": str(rundir / "paxbin" / "pax"),
        "STACK_AGENT_DATA_DIR": str(rundir / "data"),
        # The proxy holds the real key; stack-agent is given a placeholder it cannot misuse.
        "CHIP_API_KEY": "bakeoff-proxy-placeholder",
        "CARGO_TARGET_DIR": str(rundir / "target"),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_INCREMENTAL": "0",
        "NO_COLOR": "1",
    })
    out.update(pax_env)
    return out


def make_proxy(model: dict, script: "Script | list[str] | None") -> ModelProxy:
    if model["kind"] == "scripted" or script is not None:
        if not isinstance(script, Script):
            script = Script(script or [])
        return ModelProxy(mode="scripted", script=script)
    return ModelProxy(mode="forward", upstream=model["endpoint"], key=upstream_key(model),
                      model=model["model"], min_interval_s=model.get("min_interval_s", 0.0))


@dataclass
class Process:
    exit: int | None
    stdout: str
    stderr_tail: str
    wall_ms: int
    killed: str | None  # None, "harness_timeout", "fault"


def launch(cmd: list[str], cwd: Path, env: dict, timeout_s: float, proxy: ModelProxy,
           fault_watch=None, log: Path | None = None) -> Process:
    """Runs one `stack-agent` process in its own process group, so a kill takes its children too."""
    started = time.monotonic()
    err = open(log, "a") if log else subprocess.DEVNULL
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=err,
                         start_new_session=True, text=True)
    killed: list[str] = []

    def kill(reason: str):
        if killed:
            return
        killed.append(reason)
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    proxy.on_kill = lambda: kill("fault")
    out_chunks: list[str] = []
    import threading
    reader = threading.Thread(target=lambda: out_chunks.append(p.stdout.read()), daemon=True)
    reader.start()
    deadline = started + timeout_s
    while p.poll() is None:
        if fault_watch and fault_watch():
            kill("fault")
            break
        if time.monotonic() > deadline:
            # Cancellation as a person would do it: SIGTERM, then SIGKILL.
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                p.wait(10)
            except subprocess.TimeoutExpired:
                pass
            kill("harness_timeout")
            break
        time.sleep(0.005 if fault_watch else 0.1)
    p.wait()
    reader.join(5)
    if log:
        err.close()
    try:
        os.killpg(p.pid, signal.SIGKILL)  # anything left in the group (a held pax, cargo)
    except ProcessLookupError:
        pass
    tail = log.read_text()[-2000:] if log and log.exists() else ""
    return Process(exit=p.returncode, stdout="".join(out_chunks),
                   stderr_tail=tail, wall_ms=int((time.monotonic() - started) * 1000),
                   killed=killed[0] if killed else None)


def parse_report(stdout: str) -> dict | None:
    s = stdout.strip()
    if not s:
        return None
    try:
        return json.loads(s)
    except Exception:
        start = s.find("{")
        try:
            return json.loads(s[start:]) if start >= 0 else None
        except Exception:
            return None


def agent_state(env: Env, rundir: Path, task_id: str) -> dict | None:
    out = run([env.stack_agent, "agent-state", "show", "--state-db", str(rundir / "data" / "agent-state.feltdb"),
               "--task-id", task_id, "--json"], cwd=rundir / "repo")
    if out.returncode != 0:
        return None
    try:
        return json.loads(out.stdout)
    except Exception:
        return None


def goal_for(task: Task, components: dict, diagnosis: str | None) -> str:
    goal = task["goal"]
    if components["context"] == "targeted" and task.get("context", {}).get("targeted"):
        goal += "\n\nContext: " + task["context"]["targeted"]
    if components["context"] == "diagnosis" and diagnosis:
        goal += "\n\n" + diagnosis
    return goal


def diagnose(task: Task, repo: Path, target: Path) -> tuple[str | None, dict]:
    """The harness's own diagnosis before the work: which visible tests fail at the start."""
    res = cargo_tests(repo, target)
    _, visible = split_results(task, res["tests"])
    failing = sorted(k.split("::", 1)[1] for k, v in visible.items() if v == "FAILED")
    meta = {"compiled": res["compiled"], "failing": len(failing)}
    if not res["compiled"]:
        return f"Observed before this work: the project's tests do not compile ({res['error']}).", meta
    if not failing:
        return "Observed before this work: every existing test passes.", meta
    names = ", ".join(failing[:8]) + (" and more" if len(failing) > 8 else "")
    return f"Observed before this work: these tests fail: {names}.", meta


# ---- history ---------------------------------------------------------------------------------------

def seed_history(task: Task, env: Env, rundir: Path, revision: str) -> list[dict]:
    """Runs the task's scripted earlier works in this checkout, then returns it to `revision`."""
    out = []
    for i, seed in enumerate(task.get("history", {}).get("seeds", [])):
        repo = rundir / "repo"
        if seed.get("overlay"):
            apply_overlay(repo, task.part(seed["overlay"]), seed.get("remove", []))
        pax_dir = rundir / ("paxbin-alt" if seed.get("pax") == "alternate" else "paxbin")
        paxwrap.install(pax_dir)
        penv = paxwrap.env(env.pax, rundir / "pax-seed.jsonl")
        cenv = child_env(env, rundir, penv)
        cenv["PAX_BIN"] = str(pax_dir / "pax")
        with make_proxy({"kind": "scripted"}, seed_script(task, seed)) as proxy:
            cmd = [env.stack_agent, "work", seed["goal"], "--kind", seed.get("kind", task["kind"]),
                   "--endpoint", proxy.url, *model_flags({"model": "scripted-seed"}),
                   "--max-turns", "30", "--max-executions", "30", "--deadline-secs", "300",
                   "--state-db", str(rundir / "data" / "agent-state.feltdb"),
                   "--task-id", f"seed-{i}", "--lease-secs", str(LEASE_SECS), "--no-recall", "--json"]
            p = launch(cmd, repo, cenv, 360, proxy, log=rundir / "seed.log")
            writes = [c["decision"]["content_digest"] for c in proxy.calls
                      if (c.get("decision") or {}).get("capability") == "project.write"
                      and c["decision"].get("content_digest")]
        rep = parse_report(p.stdout) or {}
        out.append({"seed": seed.get("name", str(i)), "exit": p.exit, "verified": rep.get("verified"),
                    "terminal_state": rep.get("terminal_state"), "expect": seed.get("expect"),
                    "kept_tree": bool(seed.get("keep_tree")), "write_digests": writes})
        if not seed.get("keep_tree"):
            reset_to(repo, revision)
    return out


# ---- faults ----------------------------------------------------------------------------------------

class Fault:
    """A fault injected at one boundary of the first run, from the work's observed decisions."""

    BOUNDARIES = ("before_action", "after_evidence", "during_verification",
                  "after_action_before_receipt", "before_completion_persisted")

    def __init__(self, spec: dict, gate: Path):
        assert spec["boundary"] in self.BOUNDARIES, spec
        self.boundary = spec["boundary"]
        self.gate = gate
        self.wrote = False
        self.fired: dict | None = None

    def pax_mode(self) -> str:
        return {"during_verification": "hold_before",
                "after_action_before_receipt": "hold_after",
                "before_completion_persisted": "signal_after"}.get(self.boundary, "normal")

    def hook(self, index: int, decision: dict) -> str | None:
        cap = decision.get("capability")
        kind = decision.get("decision")
        if self.fired:
            return None
        if self.boundary == "before_action" and kind == "request_capability" and cap == "project.write":
            self.fired = {"call": index, "boundary": self.boundary}
            return "kill_before"
        if self.boundary == "after_evidence" and self.wrote:
            # The write was executed and its observation recorded before this call was made.
            self.fired = {"call": index, "boundary": self.boundary}
            return "kill_before"
        # The work completes itself once a test run after its change passes (Chip's policy, not the
        # model), so this boundary is reached from the verifier's side: the harness kills the work
        # as soon as that test run has exited successfully, and records afterwards whether the
        # completion had already been persisted.
        if self.boundary in ("during_verification", "after_action_before_receipt",
                             "before_completion_persisted") and self.wrote \
                and kind == "request_capability" and cap == "pax.test":
            (self.gate / "arm").write_text("1")
        if kind == "request_capability" and cap == "project.write":
            self.wrote = True
        return None

    def watch(self) -> bool:
        held = self.gate / "held"
        if held.exists() and not self.fired:
            self.fired = {"boundary": self.boundary, "held": held.read_text()}
        return held.exists()


def settle_uncertain(env: Env, rundir: Path, task_id: str, state: dict | None) -> list[dict]:
    """The operator step: settles each unresolved intent from the PAX wrapper's own log.

    The harness knows the truth (the wrapper logged whether the real test run happened), so its
    settlement is always correct. That is the best case for an operator, and is reported as such.
    """
    done = []
    if not state:
        return done
    ran = any(i.get("ran") for i in paxwrap.invocations(rundir / "pax.jsonl") if i.get("held"))
    for intent in state.get("intents", []):
        key = intent.get("key")
        if intent.get("state") == "resolved" or not key:
            continue
        out = run([env.stack_agent, "agent-state", "settle", "--state-db", str(rundir / "data" / "agent-state.feltdb"),
                   "--task-id", task_id, "--intent", key, "--executed", "true" if ran else "false",
                   "--decided-by", "bakeoff-operator", "--evidence", "the harness's PAX wrapper log",
                   "--json"], cwd=rundir / "repo")
        done.append({"intent_state": intent.get("state"), "action": (intent.get("action") or "").split(" ")[0],
                     "executed": ran, "exit": out.returncode})
    return done


# ---- the trial ---------------------------------------------------------------------------------------

def trial(*, task: Task, config: dict, model: dict, trial_index: int, env: Env, plan: str,
          stage: str, control: str | None = None, keep: bool = False) -> dict:
    comps = config["components"]
    run_id = f"{plan}.{task.id}.{config['id']}.{model['id']}.t{trial_index}"
    if control:
        run_id = f"{plan}.{task.id}.control-{control}.t{trial_index}"
    rundir = env.work_root / sha256_bytes(run_id.encode())[:16]
    if rundir.exists():
        shutil.rmtree(rundir)
    (rundir / "data").mkdir(parents=True)
    (rundir / "gate").mkdir()
    repo = rundir / "repo"
    started_ms = now_ms()
    identity = materialize(task, repo)
    revision = identity["revision"]
    paxwrap.install(rundir / "paxbin")
    record: dict = {
        "schema": RUN_SCHEMA, "run_id": run_id, "plan": plan, "stage": stage,
        "task_id": task.id, "suite": task.suite_version, "family": task["family"],
        "difficulty": task["difficulty"], "split": task["split"], "verdict_type": task["verdict"]["type"],
        "config": "control-" + control if control else config["id"],
        "components": None if control else comps, "control": control,
        "model": {"id": model["id"], "kind": model["kind"], "model": model.get("model"),
                  "profile": model.get("profile"), "endpoint_host": model.get("endpoint_host")},
        "trial": trial_index, "started_at": iso(started_ms),
        "repository": {"base_revision": revision, "fixture_digest": identity["fixture_digest"]},
        "binaries": env.identity, "budgets": task["budgets"],
    }
    # History, then any adversarial change made after it was recorded.
    if task.get("history"):
        record["history"] = seed_history(task, env, rundir, revision)
        if task["history"].get("mutate_after"):
            apply_overlay(repo, task.part(task["history"]["mutate_after"]), task["history"].get("remove_after", []))
            git(repo, "add", "-A")
            git(repo, "commit", "-q", "-m", "changed after the history was recorded")
            revision = git(repo, "rev-parse", "HEAD")
            record["repository"]["start_revision_after_mutation"] = revision
    record["repository"]["start_revision"] = revision
    diagnosis, diag_meta = (None, None)
    if not control and comps["context"] == "diagnosis":
        diagnosis, diag_meta = diagnose(task, repo, rundir / "target-diagnosis")
        shutil.rmtree(rundir / "target-diagnosis", ignore_errors=True)
    record["diagnosis"] = diag_meta
    goal = goal_for(task, comps if not control else {"context": "brief"}, diagnosis)
    record["goal_bytes"] = len(goal.encode())
    record["goal_digest"] = sha256_bytes(goal.encode())[:16]

    fault = Fault(task["fault"], rundir / "gate") if task.get("fault") and control in (None, "reference") else None
    if task.get("pax") == "missing":
        pax_env = paxwrap.env("/nonexistent/pax", rundir / "pax.jsonl")
        pax_env_note = "missing"
    else:
        pax_env = paxwrap.env(env.pax, rundir / "pax.jsonl", fault.pax_mode() if fault else "normal",
                              rundir / "gate")
        pax_env_note = "real"
    cenv = child_env(env, rundir, pax_env)
    if task.get("pax") == "missing":
        cenv["PAX_BIN"] = "/nonexistent/pax"
    record["verifier"] = pax_env_note
    durable = comps["state"] == "durable" if not control else True
    task_run_id = f"bk-{sha256_bytes(run_id.encode())[:12]}"
    b = task["budgets"]

    # A scripted "model" in a plan plays the task's reference decisions, deterministically.
    decisions = control_script(task, control or model.get("script", "reference")) \
        if control or model["kind"] == "scripted" else None
    script = Script(decisions) if decisions is not None else None
    processes, calls, settlements = [], [], []
    resume_round = 0
    while True:
        with make_proxy(model, script) as proxy:
            if fault and resume_round == 0:
                proxy.hook = fault.hook
            cmd = [env.stack_agent, "work", goal, "--kind", task["kind"], "--endpoint", proxy.url,
                   *model_flags(model if model["kind"] != "scripted" else {"model": "scripted"}),
                   "--max-turns", str(b["max_turns"]), "--max-executions", str(b["max_executions"]),
                   "--deadline-secs", str(b["deadline_secs"]), "--json"]
            if durable:
                cmd += ["--state-db", str(rundir / "data" / "agent-state.feltdb"), "--task-id", task_run_id,
                        "--lease-secs", str(LEASE_SECS)]
            else:
                cmd += ["--no-state"]
            cmd += work_flags(comps if not control else {**comps_default(), "state": "durable"})
            p = launch(cmd, repo, cenv, b["deadline_secs"] + 120, proxy,
                       fault_watch=fault.watch if fault and resume_round == 0 else None,
                       log=rundir / "work.log")
            calls.append([c for c in proxy.calls if not c.get("pending")])
        processes.append(p)
        if p.killed != "fault":
            if p.exit == 3 and durable and resume_round < MAX_RESUMES and fault and fault.fired:
                # The runtime refused to resume an action whose outcome it cannot know.
                time.sleep(LEASE_SECS + 0.5)
                settlements += settle_uncertain(env, rundir, task_run_id, agent_state(env, rundir, task_run_id))
                resume_round += 1
                if isinstance(script, Script):
                    script.reverify()
                continue
            break
        if fault and fault.fired and "persisted_before_kill" not in fault.fired and durable:
            st = agent_state(env, rundir, task_run_id) or {}
            fault.fired["persisted_before_kill"] = bool(st.get("results"))
            fault.fired["task_status_after_kill"] = (st.get("task") or {}).get("status")
        if resume_round >= MAX_RESUMES:
            break
        # Interrupted. A durable work is resumed under the same task; without state, the work is
        # simply started again, which is all an operator can do, and a scripted model starts its
        # decisions over as any model would that knows nothing of the first process.
        resume_round += 1
        if not durable and decisions is not None:
            script = Script(decisions)
        elif isinstance(script, Script):
            script.reverify()
        time.sleep(LEASE_SECS + 0.5)
        (rundir / "gate" / "arm").unlink(missing_ok=True)

    last = processes[-1]
    report = parse_report(last.stdout)
    state = agent_state(env, rundir, task_run_id) if durable else None
    record.update(
        fault=fault.fired if fault else None,
        fault_spec=task.get("fault") if fault else None,
        processes=[{"exit": p.exit, "killed": p.killed, "wall_ms": p.wall_ms} for p in processes],
        settlements=settlements,
        calls=[[slim_call(c) for c in cs] for cs in calls],
        pax_invocations=[{k: i.get(k) for k in ("kind", "test_index", "ran", "exit", "held")}
                         for i in paxwrap.invocations(rundir / "pax.jsonl")],
        report=slim_report(report),
        intents=slim_intents(state),
        stderr_tail=None if report else last.stderr_tail[-600:],
    )
    record["verification"] = verify(task, repo, record["repository"]["start_revision"], rundir)
    record["ended_at"] = iso()
    record["wall_ms"] = now_ms() - started_ms
    from .classify import classify
    record["outcome"] = classify(task, record)
    if keep:
        (rundir / "record.json").write_text(json.dumps(record, indent=2))
    else:
        shutil.rmtree(rundir, ignore_errors=True)
    return redact_value(record)


def comps_default() -> dict:
    from .configs import DEFAULTS
    return dict(DEFAULTS)


def slim_call(c: dict) -> dict:
    u = c.get("usage") or {}
    return {
        "call": c.get("call"), "status": c.get("status"), "upstream_status": c.get("upstream_status"),
        "latency_ms": c.get("latency_ms"), "paced_ms": c.get("paced_ms"), "rate_retries": c.get("rate_retries"),
        "input_tokens": u.get("prompt_tokens"), "output_tokens": u.get("completion_tokens"),
        "finish_reason": c.get("finish_reason"), "decision": c.get("decision"),
        "markers": c.get("markers"), "transport_error": c.get("transport_error"),
        "delivered": c.get("delivered", True),
    }


REPORT_KEYS = ("terminal_state", "outcome_reason", "verified", "goal_satisfied", "exit_status",
               "executions_by_capability", "malformed_replies", "invalid_decisions", "invalid_inputs",
               "capability_requests", "recoveries", "failed_observations", "write_conflicts",
               "paths_written", "tests", "pax_executions", "audit", "usage", "model", "generation",
               "model_failures")


def slim_report(r: dict | None) -> dict | None:
    if r is None:
        return None
    out = {k: r.get(k) for k in REPORT_KEYS}
    for k in ("episodes", "workflow", "agent_state", "recall"):
        if k in r:
            out[k] = r[k]
    return out


def slim_intents(state: dict | None) -> list | None:
    if not state:
        return None
    out = []
    for i in state.get("intents", []):
        out.append({k: i.get(k) for k in ("key", "state", "result", "action", "conclusion") if k in i})
    return out
