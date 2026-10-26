"""`python3 -m bakeoff <command>` (run from `bench/bakeoff`).

Commands:
  identity                         the binaries a run would use, and their versions and commits
  suite validate [--task ID ...]   freeze each task's verdict facts into validation.json
  controls [--task ID ...] --out F run every scripted control on every task (checks the harness)
  run --plan F --out F [--workers N] [--limit N]
                                   run a plan's trials through the real CLI, appending to the JSONL
  report --runs F [F ...] --out D  the decision report (report.json, report.md)
  export --runs F [F ...] --out F  one flat JSONL row per run, for comparison across runs
  reproduce --out F                rerun the deterministic earlier experiments and compare with their record
  registry validate | registry export --out F
"""

from __future__ import annotations

import argparse
import concurrent.futures as cf
import json
import os
import shutil
import sys
import tempfile
import threading
from pathlib import Path

from . import suite as suite_mod
from .configs import load_configs, load_models, model_available
from .controls import CONTROLS, EXPECTED
from .runner import Env, trial
from .util import REPO, append_jsonl, git_identity, read_json, read_jsonl, run, write_json


def make_env(work_root: Path | None = None) -> Env:
    sa = os.environ.get("STACK_AGENT_BIN") or str(REPO / "target" / "debug" / "stack-agent")
    pax = os.environ.get("BAKEOFF_PAX") or shutil.which("pax")
    if not Path(sa).is_file():
        sys.exit(f"stack-agent binary not found at {sa} (build it, or set STACK_AGENT_BIN)")
    if not pax:
        sys.exit("pax not found on PATH (or set BAKEOFF_PAX)")
    sa_version = run([sa, "--version"]).stdout.strip()
    pax_version = run([pax, "--version"]).stdout.strip()
    pax_src = os.environ.get("BAKEOFF_PAX_SRC")
    identity = {
        "stack_agent": {"version": sa_version, "source": git_identity(REPO),
                        "build": "release" if "/release/" in sa else "debug"},
        "pax": {"version": pax_version, "source": git_identity(Path(pax_src)) if pax_src else None},
        "rustc": run(["rustc", "--version"]).stdout.strip(),
    }
    root = work_root or Path(os.environ.get("BAKEOFF_WORK") or tempfile.mkdtemp(prefix="bakeoff-"))
    root.mkdir(parents=True, exist_ok=True)
    return Env(stack_agent=sa, pax=pax, work_root=root, identity=identity)


def cmd_identity(_args) -> int:
    print(json.dumps(make_env().identity, indent=2))
    return 0


def cmd_suite_validate(args) -> int:
    s = suite_mod.load_suite(args.suite)
    path = suite_mod.SUITE_ROOT / args.suite / "validation.json"
    existing = read_json(path) if path.exists() else {"tasks": {}}
    tasks = [s.task(t) for t in args.task] if args.task else s.tasks
    work = Path(tempfile.mkdtemp(prefix="bakeoff-validate-"))
    from .verify import validate_task
    bad = 0
    for t in tasks:
        facts = validate_task(t, work / t.id)
        existing["tasks"][t.id] = facts
        flag = "ok " if facts["valid"] else "BAD"
        acc_s = facts["start"]["acceptance"]
        acc_r = facts["reference"]["acceptance"]
        print(f"{flag} {t.id:42} start acc {sum(v == 'ok' for v in acc_s.values())}/{len(acc_s)}"
              f"{'' if facts['start']['compiled'] else ' (no compile)'}  reference acc "
              f"{sum(v == 'ok' for v in acc_r.values())}/{len(acc_r)}  visible "
              f"{sum(v == 'ok' for v in facts['reference']['visible'].values())}/{len(facts['reference']['visible'])}"
              + ("" if facts["valid"] else "  " + "; ".join(facts["problems"])))
        bad += not facts["valid"]
        shutil.rmtree(work / t.id, ignore_errors=True)
    existing["schema"] = "stack-agent.hard-task-validation.v1"
    existing["suite"] = args.suite
    existing["rustc"] = run(["rustc", "--version"]).stdout.strip()
    write_json(path, existing)
    shutil.rmtree(work, ignore_errors=True)
    return 1 if bad else 0


def applicable(task, control: str) -> bool:
    verdict = task["verdict"]["type"]
    targets = task.get("control_targets", {})
    # On a refusal or safe-stop task anything that changes nothing and claims nothing is correct by
    # definition, so only the reference control (the correct behavior) says anything there.
    if verdict != "change":
        return control == "reference"
    if control in ("vandal",) and not targets.get("protected_file"):
        return False
    if control in ("gamer",) and not targets.get("visible_test"):
        return False
    if control in ("wrong-fix", "vandal", "gamer") and verdict != "change":
        return False
    if task.get("fault") or task.get("history"):
        return control == "reference"
    return True


def cmd_controls(args) -> int:
    s = suite_mod.load_suite(args.suite)
    env = make_env()
    model = load_models()["scripted"]
    cfg = load_configs()["baseline"]
    tasks = [s.task(t) for t in args.task] if args.task else s.tasks
    jobs = [(t, c) for t in tasks for c in CONTROLS if applicable(t, c)]
    out = Path(args.out)
    wrong = []
    lock = threading.Lock()

    def one(job):
        t, c = job
        rec = trial(task=t, config=cfg, model=model, trial_index=0, env=env, plan="controls",
                    stage="controls", control=c)
        got = "success" if rec["outcome"]["success"] else "failure"
        want = EXPECTED[c]
        if c == "reference" and t["verdict"]["type"] == "safe_stop":
            want = "success"
        ok = got == want
        with lock:
            append_jsonl(out, rec)
            print(f"{'ok ' if ok else 'BAD'} {t.id:42} {c:14} -> {got:8} ({rec['outcome']['failure_class']})", flush=True)
            if not ok:
                wrong.append((t.id, c))
    with cf.ThreadPoolExecutor(args.workers) as ex:
        list(ex.map(one, jobs))
    print(f"{len(jobs) - len(wrong)}/{len(jobs)} controls scored as built")
    return 1 if wrong else 0


def expand_plan(plan: dict, s) -> list[tuple]:
    configs = load_configs()
    models = load_models()
    jobs = []
    for cell in plan["cells"]:
        sel = cell.get("tasks", {})
        tasks = [t for t in s.tasks
                 if (not sel.get("ids") or t.id in sel["ids"])
                 and (not sel.get("split") or t["split"] == sel["split"])
                 and (not sel.get("families") or t["family"] in sel["families"])
                 and (not sel.get("exclude_families") or t["family"] not in sel["exclude_families"])
                 and (not sel.get("tags") or set(sel["tags"]) & set(t.get("tags", [])))]
        for model_id in cell.get("models", [plan.get("model")]):
            model = models[model_id]
            for cid in cell["configs"]:
                for t in tasks:
                    for i in range(cell.get("trials", plan.get("trials", 1))):
                        jobs.append((t, configs[cid], model, i))
    seen, out = set(), []
    for j in jobs:
        key = (j[0].id, j[1]["id"], j[2]["id"], j[3])
        if key not in seen:
            seen.add(key)
            out.append(j)
    return out


def cmd_run(args) -> int:
    plan = read_json(Path(args.plan))
    s = suite_mod.load_suite(plan.get("suite", "v1"))
    jobs = expand_plan(plan, s)
    out = Path(args.out)
    done = {r["run_id"] for r in read_jsonl(out)}
    env = make_env()
    for m in {j[2]["id"]: j[2] for j in jobs}.values():
        ok, why = model_available(m)
        if not ok:
            sys.exit(f"model {m['id']} unavailable: {why}")
    todo = [j for j in jobs if f"{plan['id']}.{j[0].id}.{j[1]['id']}.{j[2]['id']}.t{j[3]}" not in done]
    if args.limit:
        todo = todo[: args.limit]
    print(f"plan {plan['id']}: {len(jobs)} trials, {len(jobs) - len(todo)} already recorded, running {len(todo)}",
          flush=True)
    lock = threading.Lock()

    def one(job):
        t, cfg, model, i = job
        try:
            rec = trial(task=t, config=cfg, model=model, trial_index=i, env=env, plan=plan["id"],
                        stage=plan.get("stage", "?"))
        except Exception as e:  # a harness failure is recorded, never silently dropped
            rec = {"schema": "stack-agent.bakeoff.run.v1",
                   "run_id": f"{plan['id']}.{t.id}.{cfg['id']}.{model['id']}.t{i}",
                   "plan": plan["id"], "stage": plan.get("stage"), "task_id": t.id, "config": cfg["id"],
                   "model": {"id": model["id"]}, "trial": i, "harness_error": f"{type(e).__name__}: {e}",
                   "outcome": {"success": False, "infra": True, "failure_class": "harness_error"}}
        o = rec["outcome"]
        with lock:
            append_jsonl(out, rec)
            print(f"{t.id:40} {cfg['id']:22} {model['id']:14} t{i} -> "
                  f"{'SUCCESS' if o['success'] else o['failure_class']:22} calls={o.get('model_calls')} "
                  f"exec={o.get('executions')} {o.get('wall_ms', 0) // 1000 if o.get('wall_ms') else '?'}s",
                  flush=True)
    with cf.ThreadPoolExecutor(args.workers) as ex:
        list(ex.map(one, todo))
    return 0


def cmd_report(args) -> int:
    from .report import build_report
    runs = [r for f in args.runs for r in read_jsonl(Path(f))]
    build_report(runs, Path(args.out), title=args.title)
    return 0


def cmd_export(args) -> int:
    from .report import flat_row
    runs = [r for f in args.runs for r in read_jsonl(Path(f))]
    out = Path(args.out)
    if out.exists():
        out.unlink()
    for r in runs:
        append_jsonl(out, flat_row(r))
    print(f"{len(runs)} rows -> {out}")
    return 0


def cmd_reproduce(args) -> int:
    from .reproduce import reproduce
    env = make_env()
    doc = reproduce(env.stack_agent, Path(args.out))
    d = doc["durability_bench"]
    e = doc["episodic_and_workflow_benches"]
    print(f"durability bench: {d.get('status')}, {d.get('changed_rows')} of {len(d.get('rows', []))} rows differ from the historical run")
    for name in ("episodic_recall", "workflow_execution"):
        c = e[name]
        print(f"{name}: {c.get('identical_cells')}/{c.get('cells')} cells identical to the committed results")
    t = doc["recovery_and_contract_tests"]
    print(f"recovery and contract tests: {t['passed']} passed, {t['failed']} failed {t['failing']}")
    return 0


def cmd_registry(args) -> int:
    from .registry import export_registry, validate_registry
    if args.action == "validate":
        problems = validate_registry()
        for p in problems:
            print(p)
        print("registry ok" if not problems else f"{len(problems)} problem(s)")
        return 1 if problems else 0
    export_registry(Path(args.out))
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(prog="bakeoff")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("identity")
    sv = sub.add_parser("suite")
    sv.add_argument("action", choices=["validate"])
    sv.add_argument("--suite", default="v1")
    sv.add_argument("--task", action="append")
    c = sub.add_parser("controls")
    c.add_argument("--suite", default="v1")
    c.add_argument("--task", action="append")
    c.add_argument("--out", required=True)
    c.add_argument("--workers", type=int, default=2)
    r = sub.add_parser("run")
    r.add_argument("--plan", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--workers", type=int, default=2)
    r.add_argument("--limit", type=int)
    rp = sub.add_parser("report")
    rp.add_argument("--runs", nargs="+", required=True)
    rp.add_argument("--out", required=True)
    rp.add_argument("--title", default="Hard-task bake-off")
    ex = sub.add_parser("export")
    ex.add_argument("--runs", nargs="+", required=True)
    ex.add_argument("--out", required=True)
    rr = sub.add_parser("reproduce")
    rr.add_argument("--out", required=True)
    rg = sub.add_parser("registry")
    rg.add_argument("action", choices=["validate", "export"])
    rg.add_argument("--out")
    args = p.parse_args(argv)
    if args.cmd == "suite":
        return cmd_suite_validate(args)
    return {"identity": cmd_identity, "controls": cmd_controls, "run": cmd_run, "report": cmd_report,
            "export": cmd_export, "registry": cmd_registry, "reproduce": cmd_reproduce}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())

