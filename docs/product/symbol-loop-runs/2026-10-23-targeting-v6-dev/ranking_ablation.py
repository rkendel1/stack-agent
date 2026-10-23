"""Offline ranking ablation on development fixtures (NOT held out).

Re-scores the candidates `rank_target` already produced, from their recorded reasons and features,
under alternative scoring variants, and reports target ranks per variant. Recall is unaffected (the
candidate sets do not change); only order does.

usage: python3 -I ranking_ablation.py FIXTURES_DIR RAW_DIR [FIXTURES_DIR RAW_DIR ...]
"""
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "scorer-v4"))
import score_v4  # noqa: E402

DEPTH = re.compile(r"^\+(\d+) (called directly by the failing test|called by a function the failing test calls|at call depth)")
EVERY_FAILING = re.compile(r"^\+(\d+) reached by every one of the")
NO_PASSING = re.compile(r"^\+(\d+) reached by none of the")
MATCH = re.compile(r"^\+(\d+) (called by its qualified path|the only function named|one of \d+ in-scope implementations|one of \d+ functions named)")
TEST_TYPE = re.compile(r"^\+(\d+) (a method of .*, a type the failing test uses|uses .*, a type the failing test uses)")


def capped_match(r):
    return min(points(r, MATCH), 5)


def points(reasons, pattern):
    return sum(int(m.group(1)) for r in reasons if (m := pattern.match(r)))


def ochiai(f):
    fr, ft, pr = f["failing_reaching"], max(f["failing_tests"], 1), f["passing_reaching"]
    return fr / math.sqrt(ft * (fr + pr)) if fr else 0.0


def flat_depth(d):
    return {0: 0, 1: 10, 2: 8, 3: 6, 4: 4}.get(d, 2)


VARIANTS = {
    "v6_baseline": lambda c, f, r: c["score"],
    "flat_depth": lambda c, f, r: c["score"] - points(r, DEPTH) + flat_depth(f["depth"]),
    "ochiai": lambda c, f, r: c["score"] + round(30 * ochiai(f)),
    "logic": lambda c, f, r: c["score"] + min(f["logic"], 10),
    "ochiai_replaces_binary": lambda c, f, r: c["score"] - points(r, EVERY_FAILING) - points(r, NO_PASSING) + round(30 * ochiai(f)),
    "flat+ochiai_replaces_binary+logic": lambda c, f, r: c["score"]
    - points(r, DEPTH)
    + flat_depth(f["depth"])
    - points(r, EVERY_FAILING)
    - points(r, NO_PASSING)
    + round(30 * ochiai(f))
    + min(f["logic"], 10),
    "flat+logic": lambda c, f, r: c["score"] - points(r, DEPTH) + flat_depth(f["depth"]) + min(f["logic"], 10),
    "flat+logic+match_capped5": lambda c, f, r: c["score"]
    - points(r, DEPTH)
    + flat_depth(f["depth"])
    + min(f["logic"], 10)
    - points(r, MATCH)
    + capped_match(r),
    "flat+logic+match_capped5+no_test_type": lambda c, f, r: c["score"]
    - points(r, DEPTH)
    + flat_depth(f["depth"])
    + min(f["logic"], 10)
    - points(r, MATCH)
    + capped_match(r)
    - points(r, TEST_TYPE),
}


def ranks(fixtures, raw_dir):
    out = []
    for name in sorted(os.listdir(fixtures)):
        gt_path = os.path.join(fixtures, name, "GROUND_TRUTH.json")
        if not os.path.isfile(gt_path):
            continue
        gt = json.load(open(gt_path))
        raw = json.load(open(os.path.join(raw_dir, f"{name}.json")))
        cands = raw["ranking"]["top"]
        for variant, fn in VARIANTS.items():
            scored = sorted(cands, key=lambda c: (-fn(c, c.get("features", {}), c["reasons"]), c["id"]))
            for t in gt["true_targets"]:
                pos = next((i + 1 for i, c in enumerate(scored) if score_v4.matches(c["id"], t)), None)
                out.append({"fixture": name, "category": gt["category"], "variant": variant, "rank": pos})
    return out


def summary(rows):
    table = {}
    for v in VARIANTS:
        for scope in ["all", "deep+dispatch", "multi+declaration"]:
            sel = [
                r["rank"]
                for r in rows
                if r["variant"] == v
                and (
                    scope == "all"
                    or (scope == "deep+dispatch" and r["category"] in ("deep", "dispatch"))
                    or (scope == "multi+declaration" and r["category"] in ("multi", "declaration"))
                )
            ]
            n = len(sel)
            found = [x for x in sel if x is not None]
            table.setdefault(v, {})[scope] = {
                "targets": n,
                "top1": round(sum(x <= 1 for x in found) / n, 3),
                "top5": round(sum(x <= 5 for x in found) / n, 3),
                "top10": round(sum(x <= 10 for x in found) / n, 3),
                "mrr": round(sum(1 / x for x in found) / n, 3),
                "median_rank_found": sorted(found)[len(found) // 2] if found else None,
            }
    return table


if __name__ == "__main__":
    a = sys.argv[1:]
    rows = [r for i in range(0, len(a), 2) for r in ranks(a[i], a[i + 1])]
    print(json.dumps({"summary": summary(rows), "rows": rows}, indent=2))
