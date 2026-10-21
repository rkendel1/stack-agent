"""Summarize harness JSONL and agent-bench trials. Usage: python3 -I summarize.py OUT_DIR"""
import collections
import glob
import json
import math
import os
import statistics
import sys

out = sys.argv[1]


def rows(path):
    if not os.path.exists(path):
        return []
    return [json.loads(l) for l in open(path) if l.startswith('{"record"')]


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, math.ceil(len(xs) * q) - 1))] if xs else None


def lat(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "n/a"
    p95 = f"{pct(xs, 0.95)/1000:.1f}" if len(xs) >= 20 else "n<20"
    return f"{statistics.median(xs)/1000:.1f} / {p95}"


print("## Layer A: basic inference (`fm respond`, greedy)\n")
b = rows(f"{out}/basic.jsonl")
if b:
    print("| Task | Successes | Categories | Median / P95 latency (s) |\n|---|---|---|---|")
    by = collections.defaultdict(list)
    for r in b:
        by[r["task_id"]].append(r)
    for k, v in by.items():
        cats = collections.Counter(r["error_category"] for r in v if r["error_category"])
        print(f"| {k} | {sum(r['success'] for r in v)}/{len(v)} | {dict(cats) or '-'} | {lat([r['latency_ms'] for r in v])} |")
    print(f"\nTotal {sum(r['success'] for r in b)}/{len(b)}.\n")

for arm in ("none", "schema"):
    s = rows(f"{out}/select-{arm}.jsonl")
    if not s:
        continue
    print(f"## Layers B/C: decisions through the real Chip loop, arm `{arm}`\n")
    print("| Kind | n | Response non-empty | Decision parses (boundary) | Correct judgment | Invocation accepted (executed) | Goal satisfied (verified) | Median / P95 latency (s) |\n|---|---|---|---|---|---|---|---|")
    for kind in ("select", "no_tool", "adversarial", None):
        v = [r for r in s if kind is None or r["kind"] == kind]
        if not v:
            continue
        n = len(v)
        c = lambda f: f"{sum(1 for r in v if f(r))}/{n}"
        print(f"| {kind or 'ALL'} | {n} | {c(lambda r: r['response_valid'])} | {c(lambda r: r['contract_valid'])} | {c(lambda r: r['tool_selection_correct'])} | {c(lambda r: r['executions'] > 0)} | {c(lambda r: r['goal_satisfied'])} | {lat([r['latency_ms'] for r in v])} |")
    cats = collections.Counter(r["error_category"] or "none" for r in s)
    print(f"\nFirst-decision outcome: {dict(cats)}")
    print(f"First decision kinds: {dict(collections.Counter(r['first_decision'] for r in s))}")
    print(f"Executions of an undeclared capability: {sum(1 for r in s for c in r['executed_capabilities'] if not c.startswith('compute.'))}; reality without execution: {sum(r['reality_without_execution'] for r in s)}\n")

for label in ("apple", "baseline"):
    files = glob.glob(f"{out}/agent-bench-{label}/trials/*.json")
    if not files:
        continue
    print(f"## Real runtime: `stack-agent --agent-bench`, local-none arm, {label}\n")
    print("| Task | Attempted | Verified complete | First-reply contract valid | Failure kinds |\n|---|---|---|---|---|")
    by = collections.defaultdict(list)
    for f in files:
        d = json.load(open(f))
        by[d["task"]].append(d)
    for k in sorted(by):
        v = by[k]
        valid = sum(1 for d in v if d["replies_received"] and d["replies_schema_valid"] == d["replies_received"])
        print(f"| {k} | {len(v)} | {sum(bool(d['task_complete']) for d in v)}/{len(v)} | {valid}/{len(v)} | {dict(collections.Counter(d['failure_kind'] for d in v))} |")
    print()
