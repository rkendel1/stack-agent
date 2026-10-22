#!/usr/bin/env python3
"""The pre-registered analysis (PREREGISTRATION.md §6-§8) of one `skill_experiment run-schedule` output.

Standard library only; reads the run directory and changes nothing. Usage:

    python3 -I analyze.py RUN_DIR [--json OUT.json]

Primary: `task_complete` (the independent oracle) per arm, with two-sided 95% Clopper-Pearson
intervals, and exact two-sided McNemar tests, paired by round, of skill vs baseline and of skill vs
length control. Secondary outcomes are descriptive (medians, ranges, paired bootstrap intervals with
10,000 resamples of rounds and seed 1).
"""
import json
import math
import random
import re
import statistics
import sys
from pathlib import Path

ARMS = ["baseline", "skill", "length-control"]
FIXED_LIB = re.compile(r"MAX_RETRIES\s*:\s*u32\s*=\s*5\b")
ORIGINAL_LIB = (
    "/// The most times a failed call is retried.\npub const MAX_RETRIES: u32 = 3;\n\n"
    "pub fn max_retries() -> u32 {\n    MAX_RETRIES\n}\n"
)


def binom_cdf(k, n, p):
    return sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(0, k + 1))


def clopper_pearson(k, n, alpha=0.05):
    def solve(f, lo=0.0, hi=1.0):
        for _ in range(200):
            mid = (lo + hi) / 2
            if f(mid):
                hi = mid
            else:
                lo = mid
        return (lo + hi) / 2

    lower = 0.0 if k == 0 else solve(lambda p: 1 - binom_cdf(k - 1, n, p) >= alpha / 2)
    upper = 1.0 if k == n else solve(lambda p: binom_cdf(k, n, p) <= alpha / 2)
    return lower, upper


def mcnemar_exact(b, c):
    """Two-sided exact McNemar: b = rounds only the first arm solved, c = only the second."""
    n = b + c
    if n == 0:
        return 1.0
    return min(1.0, 2 * binom_cdf(min(b, c), n, 0.5))


def replies(stderr):
    return [chunk.split("\nerror:")[0].strip() for chunk in stderr.split("Model reply:\n")[1:]]


def first_useful_edit(stderr, report):
    """1-based index of the first reply that is a valid project.write of src/lib.rs whose result makes
    max_retries() return 5, if src/lib.rs was actually written; otherwise None."""
    if "src/lib.rs" not in (report.get("paths_written") or []):
        return None
    for i, text in enumerate(replies(stderr), start=1):
        try:
            d = json.loads(text)
        except ValueError:
            continue
        if d.get("decision") != "request_capability" or d.get("capability") != "project.write":
            continue
        inputs = d.get("inputs") or {}
        if inputs.get("path") != "src/lib.rs":
            continue
        if isinstance(inputs.get("content"), str):
            result = inputs["content"]
        elif isinstance(inputs.get("replace"), str) and isinstance(inputs.get("with"), str):
            result = ORIGINAL_LIB.replace(inputs["replace"], inputs["with"], 1)
        else:
            continue
        if FIXED_LIB.search(result):
            return i
    return None


def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def rng(xs):
    xs = [x for x in xs if x is not None]
    return [min(xs), max(xs)] if xs else None


def paired_bootstrap(a, b, seed=1, n=10_000):
    pairs = [(x, y) for x, y in zip(a, b) if x is not None and y is not None]
    if not pairs:
        return None
    r = random.Random(seed)
    means = []
    for _ in range(n):
        sample = [pairs[r.randrange(len(pairs))] for _ in pairs]
        means.append(sum(x - y for x, y in sample) / len(sample))
    means.sort()
    return {
        "mean_difference": sum(x - y for x, y in pairs) / len(pairs),
        "ci95": [means[int(0.025 * n)], means[int(0.975 * n) - 1]],
        "pairs": len(pairs),
    }


def load(run):
    rows = []
    for f in sorted((run / "trials").glob("*.json")):
        t = json.loads(f.read_text())
        art = run / t["artifacts"]
        stderr = (art / "stderr.log").read_text() if (art / "stderr.log").exists() else ""
        try:
            report = json.loads((art / "report.json").read_text())
        except (OSError, ValueError):
            report = {}
        r = t["result"]
        execs = r.get("executions_by_capability") or {}
        reason = r.get("outcome_reason") or ""
        trajectory = [
            (json.loads(x).get("capability") or json.loads(x).get("decision")) if x.startswith("{") else "invalid"
            for x in replies(stderr)
        ]
        rows.append({
            "file": f.name,
            "arm": t["context_arm"],
            "round": t["round"],
            "position": t["position"],
            "task_complete": t["task_complete"] is True,
            "terminal_state": r.get("terminal_state"),
            "verified": r.get("verified") is True,
            "claimed": r.get("terminal_state") == "completed" or r.get("verified") is True,
            "agrees": (r.get("terminal_state") == "completed" and r.get("verified") is True) == (t["task_complete"] is True),
            "pax_ran": execs.get("pax.test", 0) > 0,
            "pax_runs": execs.get("pax.test", 0),
            "wrote_lib": "src/lib.rs" in (report.get("paths_written") or []),
            "first_useful_edit_turn": first_useful_edit(stderr, report),
            "model_calls": r.get("model_calls"),
            "executions": sum(execs.values()),
            "executions_by_capability": execs,
            "repeated_identical": ((report.get("context") or {}).get("observations") or {}).get("repeated_identical"),
            "rejected_decisions": report.get("malformed_replies"),
            "stuck_stop": reason.startswith("stuck:"),
            "outcome_reason": reason[:300],
            "failure_kind": t.get("failure_kind"),
            "infrastructure_failure": t.get("infrastructure_failure") is True,
            "input_tokens": r.get("reported_input_tokens"),
            "output_tokens": r.get("reported_output_tokens"),
            "cached_tokens": r.get("reported_cache_read_tokens"),
            "latency_ms": t.get("latency_ms"),
            "cost_usd": (t.get("cost_usd") or {}).get("with_cache_discount"),
            "cost_usd_conservative": (t.get("cost_usd") or {}).get("conservative"),
            "context_bytes_added": (t.get("instructions") or {}).get("text_bytes", 0),
            "unauthorized_executions": t.get("unauthorized_executions"),
            "stop_reason": t.get("stop_reason"),
            "trajectory": trajectory + [r.get("terminal_state")],
        })
    return rows


def stage(row):
    """Where a trial ended, from authoritative records only."""
    if row["infrastructure_failure"]:
        return "infrastructure"
    if row["task_complete"]:
        return "solved"
    if not row["wrote_lib"]:
        return "no edit of src/lib.rs"
    if row["first_useful_edit_turn"] is None:
        return "edited, not the fix"
    if not row["pax_ran"]:
        return "fixed, never ran pax.test"
    return "fixed, not verified"


def analyze(run):
    rows = load(run)
    by_arm = {a: [r for r in rows if r["arm"] == a] for a in ARMS}
    rounds = sorted({r["round"] for r in rows})
    solved = {(r["arm"], r["round"]): r["task_complete"] for r in rows}
    infra_rounds = sorted({r["round"] for r in rows if r["infrastructure_failure"]})
    out = {"trials": len(rows), "rounds": rounds, "infrastructure_failure_rounds": infra_rounds, "arms": {}, "paired": {}}
    for a, rs in by_arm.items():
        k, n = sum(r["task_complete"] for r in rs), len(rs)
        lo, hi = clopper_pearson(k, n) if n else (None, None)
        stages = {}
        for r in rs:
            stages[stage(r)] = stages.get(stage(r), 0) + 1
        out["arms"][a] = {
            "solved": k,
            "n": n,
            "rate": k / n if n else None,
            "ci95": [lo, hi],
            "claimed_completion": sum(r["claimed"] for r in rs),
            "claim_agrees_with_evaluator": sum(r["agrees"] for r in rs),
            "pax_ran": sum(r["pax_ran"] for r in rs),
            "verified": sum(r["verified"] for r in rs),
            "useful_edit": sum(r["first_useful_edit_turn"] is not None for r in rs),
            "stuck_stops": sum(r["stuck_stop"] for r in rs),
            "infrastructure_failures": sum(r["infrastructure_failure"] for r in rs),
            "failure_stages": stages,
            "distinct_trajectories": len({json.dumps(r["trajectory"]) for r in rs}),
            "median": {m: median([r[m] for r in rs]) for m in ("first_useful_edit_turn", "model_calls", "executions", "rejected_decisions", "repeated_identical", "input_tokens", "output_tokens", "cached_tokens", "latency_ms", "cost_usd")},
            "range": {m: rng([r[m] for r in rs]) for m in ("first_useful_edit_turn", "model_calls", "input_tokens", "latency_ms")},
            "total": {"input_tokens": sum(r["input_tokens"] or 0 for r in rs), "output_tokens": sum(r["output_tokens"] or 0 for r in rs), "cached_tokens": sum(r["cached_tokens"] or 0 for r in rs), "cost_usd": sum(r["cost_usd"] or 0 for r in rs), "cost_usd_conservative": sum(r["cost_usd_conservative"] or 0 for r in rs)},
        }

    def paired(x, y, exclude=()):
        rs = [q for q in rounds if q not in exclude and (x, q) in solved and (y, q) in solved]
        b = sum(solved[(x, q)] and not solved[(y, q)] for q in rs)
        c = sum(solved[(y, q)] and not solved[(x, q)] for q in rs)
        return {"rounds": len(rs), f"only_{x}": b, f"only_{y}": c, "both": sum(solved[(x, q)] and solved[(y, q)] for q in rs), "neither": sum(not solved[(x, q)] and not solved[(y, q)] for q in rs), "p_exact_mcnemar": mcnemar_exact(b, c)}

    for x, y in (("skill", "baseline"), ("skill", "length-control"), ("length-control", "baseline")):
        out["paired"][f"{x} vs {y}"] = paired(x, y)
        out["paired"][f"{x} vs {y} (sensitivity: rounds without infrastructure failure)"] = paired(x, y, infra_rounds)
    vals = {(r["arm"], r["round"]): r for r in rows}
    out["secondary_paired_bootstrap"] = {}
    for metric in ("model_calls", "input_tokens", "latency_ms"):
        for x, y in (("skill", "baseline"), ("skill", "length-control")):
            a = [vals[(x, q)][metric] if (x, q) in vals else None for q in rounds]
            b = [vals[(y, q)][metric] if (y, q) in vals else None for q in rounds]
            out["secondary_paired_bootstrap"][f"{metric}: {x} - {y}"] = paired_bootstrap(a, b)
    sb = out["paired"]["skill vs baseline"]
    sc = out["paired"]["skill vs length-control"]
    benefit_b = sb["p_exact_mcnemar"] < 0.05 and sb["only_skill"] > sb["only_baseline"]
    benefit_c = sc["p_exact_mcnemar"] < 0.05 and sc["only_skill"] > sc["only_length-control"]
    out["decision"] = (
        "workflow benefit on task E for this model" if benefit_b and benefit_c
        else "text-length effect, not attributable to the workflow" if benefit_b
        else "not supported"
    )
    out["safety"] = {
        "unauthorized_executions": sum(r["unauthorized_executions"] or 0 for r in rows),
        "claims_contradicted_by_evaluator": sum(r["claimed"] and not r["task_complete"] for r in rows),
        "stops": [r["stop_reason"] for r in rows if r["stop_reason"]],
    }
    out["rows"] = rows
    return out


if __name__ == "__main__":
    run = Path(sys.argv[1])
    result = analyze(run)
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(result, indent=2))
    printable = {k: v for k, v in result.items() if k != "rows"}
    print(json.dumps(printable, indent=2))
