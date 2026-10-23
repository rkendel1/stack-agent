"""Development diagnostics for targeting v6 (NOT a held-out evaluation).

For each fixture set already ranked by scorer v4 (`score_v4.py run`), reports:
- candidate recall, rank conditional on being found, top-1/5/10 recall and MRR, per category;
- for every miss, whether the frontier explains it, by the rule `target::explain_miss` applies:
  the missed function is in `hidden_reach` (some material unresolved edge may reach it), or in
  `cutoff_reach` after a budget stop (or the reach beyond the cut-off was not fully computed), or in
  `unobserved_reach` (tests that never ran because their file does not compile). Anything else,
  including an unrelated warning, is unexplained;
- for fixtures with documented paths (v5), whether the explaining edge sits on that path;
- the material unresolved-edge rate, budget stops, candidate counts and runtimes.

usage: python3 -I dev_analyze.py LABEL FIXTURES_DIR RAW_DIR [LABEL FIXTURES_DIR RAW_DIR ...]
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "scorer-v4"))
import score_v4  # noqa: E402


def target_id(raw, t):
    """The candidate-style id a ground-truth target corresponds to in this output, if any."""
    for cid in list(raw["ranking"]["frontier"].get("hidden_reach", {}).keys()) + raw["ranking"]["frontier"].get(
        "cutoff_reach", []
    ) + raw["ranking"]["frontier"].get("unobserved_reach", []):
        try:
            if score_v4.matches(cid, t):
                return cid
        except score_v4.BadId:
            pass
    return None


def explain(raw, t):
    f = raw["ranking"]["frontier"]
    cid = target_id(raw, t)
    if cid and cid in f.get("hidden_reach", {}):
        e = f["material"][f["hidden_reach"][cid][0]]
        return ("explained", f'unresolved {e["call"]} at {e["path"]}:{e["line"]} in {e["from"]}: {e["cause"]}', e)
    if f["stop"] != "complete":
        if cid and cid in f.get("cutoff_reach", []):
            return ("explained", f'reachable beyond the {f["stop"]} cut-off', None)
        if f.get("reach_capped"):
            return ("explained", f'{f["stop"]} cut-off; reach beyond it not fully computed', None)
    if cid and cid in f.get("unobserved_reach", []):
        return ("explained", "reachable only from tests that never ran (test file does not compile)", None)
    return ("unexplained", "nothing in the frontier connects to it", None)


def metrics(ranks):
    n = len(ranks)
    found = [r for r in ranks if r is not None]
    if not n:
        return None
    return {
        "targets": n,
        "found": len(found),
        "recall": round(len(found) / n, 3),
        "top1": round(sum(r <= 1 for r in found) / n, 3),
        "top5": round(sum(r <= 5 for r in found) / n, 3),
        "top10": round(sum(r <= 10 for r in found) / n, 3),
        "mrr": round(sum(1 / r for r in found) / n, 3),
        "mean_rank_found": round(sum(found) / len(found), 1) if found else None,
        "median_rank_found": sorted(found)[len(found) // 2] if found else None,
    }


def analyze(label, fixtures, raw_dir):
    v4 = json.load(open(os.path.join(raw_dir, "results-v4.json")))
    rows, by_cat = [], {}
    for r in v4["fixtures"]:
        name = r["fixture"]
        if "error" in r:
            rows.append({"fixture": name, "error": r["error"]})
            continue
        gt = json.load(open(os.path.join(fixtures, name, "GROUND_TRUTH.json")))
        raw = json.load(open(os.path.join(raw_dir, f"{name}.json")))
        f = raw["ranking"]["frontier"]
        misses = []
        for i, t in enumerate(r["targets"]):
            if t["rank"] is not None:
                continue
            verdict, why, edge = explain(raw, gt["true_targets"][i])
            on_path = None
            paths = [p for p in gt.get("paths", []) if p.get("target") == i]
            if edge and paths:
                on_path = any(s["function"] == edge["from"] for p in paths for s in p["steps"])
            misses.append({"target": t["target"], "verdict": verdict, "why": why, "edge_on_documented_path": on_path})
        by_cat.setdefault(gt["category"], []).extend(t["rank"] for t in r["targets"])
        rows.append(
            {
                "fixture": name,
                "category": gt["category"],
                "tags": gt.get("tags", []),
                "decision": r["decision"],
                "ranks": [t["rank"] for t in r["targets"]],
                "candidates": r["candidates"],
                "stop": r["stop"],
                "material_edges": len(f.get("material", [])),
                "benign_edges": f.get("benign"),
                "could_conceal": r["could_conceal"],
                "rank_ms": r["rank_ms"],
                "index_ms": r["index_ms"],
                "misses": misses,
            }
        )
    ok = [x for x in rows if "error" not in x]
    all_misses = [m for x in ok for m in x["misses"]]
    return {
        "label": label,
        "scorer_v4_criteria": v4["criteria"],
        "by_category": {c: metrics(v) for c, v in sorted(by_cat.items())},
        "all": metrics([r for v in by_cat.values() for r in v]),
        "misses": len(all_misses),
        "misses_explained": sum(m["verdict"] == "explained" for m in all_misses),
        "misses_unexplained": [f'{x["fixture"]}: {m["target"]}' for x in ok for m in x["misses"] if m["verdict"] != "explained"],
        "fixtures_with_material_edges": f'{sum(x["material_edges"] > 0 for x in ok)}/{len(ok)}',
        "budget_stops": [x["fixture"] for x in ok if x["stop"] != "complete"],
        "single_decisions": [x["fixture"] for x in ok if x["decision"] == "single"],
        "max_rank_ms": max((x["rank_ms"] for x in ok), default=None),
        "fixtures": rows,
    }


if __name__ == "__main__":
    args = sys.argv[1:]
    out = [analyze(*args[i : i + 3]) for i in range(0, len(args), 3)]
    print(json.dumps(out, indent=2))
