"""Stage 1: rerun the earlier deterministic experiments at this commit and compare with their record.

Only experiments whose configuration can be reconstructed exactly are rerun here: they use a
scripted model and no network. Each comparison says which metrics are the same, which changed and
why that may be (later fixes), and never re-scores the original. Real-model experiments are not
reconstructable from this environment (other providers, local GPU servers); the registry records
them as historical baselines.
"""

from __future__ import annotations

import os
import re
import tempfile
from pathlib import Path

from .util import REPO, git_identity, iso, read_json, run, write_json


def _cargo_test(args: list[str], env: dict | None = None, timeout: int = 3600) -> dict:
    e = {**os.environ, "CARGO_INCREMENTAL": "0", **(env or {})}
    out = run(["cargo", "test", *args], cwd=REPO, env=e, timeout=timeout)
    passed = sum(int(m) for m in re.findall(r"test result: \w+\. (\d+) passed", out.stdout))
    failed = sum(int(m) for m in re.findall(r"; (\d+) failed", out.stdout))
    failing = re.findall(r"^test (\S+) \.\.\. FAILED", out.stdout, re.M)
    return {"exit": out.returncode, "passed": passed, "failed": failed, "failing": failing}


def durability(stack_agent: str) -> dict:
    committed = read_json(REPO / "docs/product/durability-bench/run1-deterministic/summary.json")
    with tempfile.TemporaryDirectory() as tmp:
        out = run([stack_agent, "--durability-bench", "--repeats", "20", "--out", tmp], cwd=REPO, timeout=3600)
        now_path = Path(tmp) / "summary.json"
        if out.returncode != 0 or not now_path.exists():
            return {"status": "failed", "exit": out.returncode, "stderr": out.stderr[-500:]}
        now = read_json(now_path)
    keys = ("verified_complete", "duplicate_effects", "false_successes", "operator_interventions", "safe_refusals")

    def index(doc):
        return {(r["task"], r["scenario"]): r for r in doc["rows"]}

    old, new = index(committed), index(now)
    rows = []
    for k in sorted(set(old) | set(new)):
        o, n = old.get(k), new.get(k)
        row = {"task": k[0], "scenario": k[1]}
        for arm in ("A", "B"):
            row[arm] = {m: {"then": (o or {}).get(arm, {}).get(m), "now": (n or {}).get(arm, {}).get(m)} for m in keys}
        row["changed"] = any(row[a][m]["then"] != row[a][m]["now"] for a in ("A", "B") for m in keys)
        rows.append(row)
    return {"status": "ran", "historical_artifact": "docs/product/durability-bench/run1-deterministic/summary.json",
            "note": "The historical run predates fixes P1-P3; a change in arm B is expected where those fixes apply.",
            "rows": rows, "changed_rows": sum(r["changed"] for r in rows)}


def episodic() -> dict:
    with tempfile.TemporaryDirectory() as tmp:
        ep, wf = Path(tmp) / "episodic.json", Path(tmp) / "workflow.json"
        res = _cargo_test(["-p", "stack-agent", "--test", "episodic_recall_bench"],
                          env={"EPISODIC_BENCH_OUT": str(ep), "WORKFLOW_BENCH_OUT": str(wf)})
        now_ep = read_json(ep) if ep.exists() else None
        now_wf = read_json(wf) if wf.exists() else None
    then_ep = read_json(REPO / "docs/product/episodic-memory-bench/results.json")
    then_wf = read_json(REPO / "docs/product/workflow-execution-bench/results.json")
    keys_ep = ("verified_success", "model_calls_per_verified_run", "executions_per_verified_run", "useful_reuse",
               "incorrect_reuse", "safety_regressions")
    keys_wf = ("verified_success", "model_calls_per_verified_run", "executions_per_verified_run",
               "duplicate_side_effects", "followed", "fallback_correct", "safety_regressions")

    def compare(then, now, key_fields, keys):
        if not now:
            return {"status": "not produced"}
        idx = lambda d: {tuple(r.get(f) for f in key_fields): r for r in d["summary"]}
        o, n = idx(then), idx(now)
        rows = []
        for k in sorted(set(o) | set(n), key=str):
            row = {"cell": "/".join(str(x) for x in k)}
            row.update({m: {"then": o.get(k, {}).get(m), "now": n.get(k, {}).get(m)} for m in keys})
            row["same"] = all(row[m]["then"] == row[m]["now"] for m in keys)
            rows.append(row)
        return {"rows": rows, "identical_cells": sum(r["same"] for r in rows), "cells": len(rows)}

    return {"tests": res,
            "episodic_recall": compare(then_ep, now_ep, ("scenario", "arm"), keys_ep),
            "workflow_execution": compare(then_wf, now_wf, ("condition",), keys_wf)}


def recovery_tests() -> dict:
    return _cargo_test(["-p", "stack-agent", "--test", "kill_boundaries", "--test", "crash_windows",
                        "--test", "reconciliation", "--test", "project_write_reconciliation",
                        "--test", "consequential_recovery", "--test", "malformed_reply_recovery",
                        "--test", "completion_evidence", "--test", "cancellation"])


def reproduce(stack_agent: str, out: Path) -> dict:
    doc = {"schema": "stack-agent.bakeoff.reproduction.v1", "ran_at": iso(), "source": git_identity(REPO),
           "durability_bench": durability(stack_agent), "episodic_and_workflow_benches": episodic(),
           "recovery_and_contract_tests": recovery_tests()}
    write_json(out, doc)
    return doc

