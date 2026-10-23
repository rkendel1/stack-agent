"""Evaluator's validity checks 1 and 2 (PREREGISTRATION.md), run before any ranking.

1. Structure: 12 fixtures, 3 per category, every required tag present, required ground-truth fields,
   a documented path ending at each target.
2. Reference fix: in fresh copies with separate build directories, `cargo test --offline` fails
   before the fix and passes every test after it.

usage: python3 -I verify_fixtures.py FIXTURES_DIR WORK_DIR OUT_JSON
"""
import json
import os
import re
import shutil
import subprocess
import sys

CATEGORIES = ["deep", "dispatch", "multi", "declaration"]
TAGS = ["producer_consumer", "implicit_dispatch", "factory_or_helper_type", "uncompiled_test", "ambiguous", "large_search"]
FIELDS = ["fixture", "category", "fix_kind", "true_targets", "acceptance", "tags", "paths"]


def structure(name, gt):
    problems = [f"missing field {f}" for f in FIELDS if f not in gt]
    if problems:
        return problems
    if gt["category"] not in CATEGORIES:
        problems.append(f'unknown category {gt["category"]}')
    for i, t in enumerate(gt["true_targets"]):
        paths = [p for p in gt["paths"] if p.get("target") == i]
        if not paths:
            problems.append(f"no documented path to target {i}")
        for p in paths:
            last = p["steps"][-1] if p.get("steps") else {}
            if (last.get("file"), last.get("function")) != (t["file"], t["function"]):
                problems.append(f"a path to target {i} does not end at it")
    return problems


def cargo(root, target, log):
    p = subprocess.run(
        ["cargo", "test", "--offline"],
        cwd=root,
        capture_output=True,
        text=True,
        env=dict(os.environ, CARGO_TARGET_DIR=target),
    )
    open(log, "w").write(p.stdout + p.stderr)
    return p.returncode, p.stdout + p.stderr


def main(fixtures, work, out_json):
    os.makedirs(work, exist_ok=True)
    logs = os.path.join(os.path.dirname(os.path.abspath(out_json)), "verify-logs")
    os.makedirs(logs, exist_ok=True)
    report, cats, tags = [], {}, set()
    for name in sorted(os.listdir(fixtures)):
        src = os.path.join(fixtures, name)
        gt_path = os.path.join(src, "GROUND_TRUTH.json")
        if not os.path.isdir(src) or not os.path.isfile(gt_path):
            continue
        gt = json.load(open(gt_path))
        entry = {"fixture": name, "category": gt.get("category"), "tags": gt.get("tags", [])}
        entry["structure_problems"] = structure(name, gt)
        cats[gt.get("category")] = cats.get(gt.get("category"), 0) + 1
        tags.update(gt.get("tags", []))
        before, after = os.path.join(work, name + "-before"), os.path.join(work, name + "-after")
        for d in (before, after):
            shutil.rmtree(d, ignore_errors=True)
            shutil.copytree(src, d, ignore=shutil.ignore_patterns("target"))
        patched = subprocess.run(["patch", "-p1", "-s", "-i", "solution.patch"], cwd=after, capture_output=True, text=True)
        entry["patch_applies"] = patched.returncode == 0
        b, b_out = cargo(before, os.path.join(work, name + "-tb"), os.path.join(logs, f"{name}-before.log"))
        a, a_out = cargo(after, os.path.join(work, name + "-ta"), os.path.join(logs, f"{name}-after.log"))
        entry["before_exit"], entry["after_exit"] = b, a
        entry["before_failing"] = re.findall(r"^test (\S+) \.\.\. FAILED$", b_out, re.M)
        entry["before_compile_errors"] = len(re.findall(r"^error(\[E\d+\])?:", b_out, re.M))
        entry["after_results"] = [l for l in a_out.splitlines() if l.startswith("test result:")]
        entry["valid"] = (
            not entry["structure_problems"]
            and entry["patch_applies"]
            and b != 0
            and a == 0
            and all("test result: ok." in l for l in entry["after_results"])
        )
        for d in (before, after, os.path.join(work, name + "-tb"), os.path.join(work, name + "-ta")):
            shutil.rmtree(d, ignore_errors=True)
        report.append(entry)
    summary = {
        "fixtures": len(report),
        "per_category": cats,
        "categories_balanced": all(cats.get(c) == 3 for c in CATEGORIES) and len(report) == 12,
        "missing_tags": [t for t in TAGS if t not in tags],
        "invalid": [e["fixture"] for e in report if not e["valid"]],
        "entries": report,
    }
    open(out_json, "w").write(json.dumps(summary, indent=2))
    print(json.dumps({k: v for k, v in summary.items() if k != "entries"}, indent=2))


if __name__ == "__main__":
    main(*sys.argv[1:4])
