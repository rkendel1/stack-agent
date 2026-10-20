#!/usr/bin/env python3
"""Can the stuck guard have fired on the recorded real-model trials? (stdlib only)

Input: docs/product/agent-bench/*/trials/*.json, 243 trials of `stack-agent work` by a local model
(gemma), OpenAI (gpt-5.5) and Anthropic (claude-sonnet-5-5) on five tasks.

What a trial record holds: the capability NAMES requested (`requested_capabilities`), execution
counts by capability, and, for the workspace-inspection task, a coverage summary. It holds NO
request inputs and NO observation text. The guard compares a request's complete inputs and what it
observed, so it cannot be replayed exactly. What can be established is a necessary condition:

  * The guard needs at least 3 observations, and 3 consecutive requests with the same capability
    (repeated) or a 4-request X Y X Y cycle of capabilities (alternating). If a trial does not
    contain such a name sequence, the guard cannot have fired on it.
  * For the workspace task, the recorded coverage gives a lower bound on how many DISTINCT files
    were read (`read_candidates` manifests + the root manifest + the overview when read). The
    guard needs two duplicate requests to fire (three identical, or X Y X Y); if the reads exceed
    that lower bound by at most one, it cannot have fired.
  * Anything else is "insufficient evidence": the trial may or may not have repeated an identical
    request, and the record cannot say.

Assumption: `read_candidates`, the root manifest and the overview are different files, each read
at least once. The classes are assigned from the record, by rule, below.
"""
import collections
import glob
import json
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "..", "product", "agent-bench"))


def name_rep(s):
    return any(s[i] == s[i + 1] == s[i + 2] for i in range(len(s) - 2))


def name_alt(s):
    return any(s[i] != s[i + 1] and s[i] == s[i + 2] and s[i + 1] == s[i + 3] for i in range(len(s) - 3))


def classify(d):
    r = d.get("result") or {}
    by = r.get("executions_by_capability") or {}
    execs = sum(by.values())
    seq = d.get("requested_capabilities")
    shape = None
    if execs < 3:
        return "cannot_fire", "fewer than 3 observations", execs, None
    if seq is None:
        return "insufficient", "no request sequence recorded", execs, None
    rep, alt = name_rep(seq), name_alt(seq)
    if not (rep or alt):
        return "cannot_fire", "no name-level repeat or cycle", execs, None
    shape = "repeated" if rep else "alternating"
    if d["task_id"] == "cargo_workspace_inspection":
        cov = r.get("inspection_coverage") or {}
        reads, lists = seq.count("project.read"), seq.count("project.list")
        distinct_lower = (cov.get("read_candidates") or 0) + int(bool(cov.get("root_manifest_read"))) + int(bool(cov.get("overview_read")))
        duplicates_upper = max(0, reads - distinct_lower) + max(0, lists - 1)
        if cov and duplicates_upper <= 1:
            return "repetitive_progress", f"{reads} reads of at least {distinct_lower} distinct files (<= {duplicates_upper} duplicate)", execs, shape
    return "insufficient", "name-level repeat; inputs and observations not recorded", execs, shape


def main():
    rows = []
    for f in sorted(glob.glob(os.path.join(ROOT, "*", "trials", "*.json"))):
        d = json.load(open(f))
        cls, why, execs, shape = classify(d)
        rows.append({"run": f.split(os.sep)[-3], "trial": os.path.basename(f), "provider": d["provider"],
                     "task": d["task_id"], "task_complete": d.get("task_complete"),
                     "terminal_state": (d.get("result") or {}).get("terminal_state"),
                     "class": cls, "why": why, "executions": execs, "name_pattern": shape,
                     "requests": len(d.get("requested_capabilities") or [])})
    n = len(rows)
    cnt = collections.Counter(r["class"] for r in rows)
    print(f"trials: {n}; skipped: 0")
    for k in ("cannot_fire", "repetitive_progress", "insufficient"):
        print(f"  {k}: {cnt[k]}")
    print("by provider (cannot_fire / repetitive_progress / insufficient):")
    for p in sorted({r["provider"] for r in rows}):
        c = collections.Counter(r["class"] for r in rows if r["provider"] == p)
        print(f"  {p}: {c['cannot_fire']} / {c['repetitive_progress']} / {c['insufficient']}")
    print("insufficient trials:")
    for r in rows:
        if r["class"] == "insufficient":
            print(f"  {r['run']} {r['provider']} {r['task']} requests={r['requests']} pattern={r['name_pattern']} complete={r['task_complete']} {r['why']}")
    out = sys.argv[1] if len(sys.argv) > 1 else None
    if out:
        json.dump({"trials": n, "counts": cnt, "rows": rows}, open(out, "w"), indent=1)


if __name__ == "__main__":
    main()
