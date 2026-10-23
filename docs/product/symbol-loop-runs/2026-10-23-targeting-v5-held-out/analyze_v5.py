"""Supplementary analysis for the v5 held-out evaluation (see PREREGISTRATION.md).

Reads the fixtures' GROUND_TRUTH.json and the raw outputs and results-v4.json that scorer v4 wrote,
and computes what scorer v4 does not: P1 against independently documented material omissions, P2
on ambiguous fixtures, P6, P7 and P8, and the overall decision. It never reruns ranking and never
changes scorer v4's results.

usage: python3 -I analyze_v5.py FIXTURES_DIR RAW_DIR
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "scorer-v4"))
import score_v4  # noqa: E402


def is_test_file(path):
    return path.startswith("tests/") or "/tests/" in path


def step_id(step):
    return f'symbol:{step["file"]}:{step["function"]}'


def names(ids, step):
    """Whether any id in `ids` names `step`."""
    t = {"file": step["file"], "function": step["function"]}
    for i in ids:
        try:
            if score_v4.matches(i, t):
                return True
        except score_v4.BadId:
            pass
    return False


def material_report(path, out):
    """For a documented path to a missed target: (reported, reason, gap, predecessor).

    The gap is the first production step that is not a candidate; the predecessor is the step
    before it. The omission counts as reported only when the frontier says the search could conceal
    the target AND one of: (a) a budget stopped the walk; (b) a concealment names the path's test
    file as not compiling; (c) the target or the gap function is listed as hidden or as a
    plausible target of an excluded call; (d) an excluded call, or a concealment example, comes from
    the predecessor function.
    """
    r = out["ranking"]
    f = r["frontier"]
    cands = [c["id"] for c in r["top"]]
    steps = path["steps"]
    gap, pred = None, None
    for i, s in enumerate(steps[1:], start=1):
        if is_test_file(s["file"]):
            continue
        if not names(cands, s):
            gap, pred = s, steps[i - 1]
            break
    if gap is None:
        return (True, "every production step is a candidate", None, None)
    if not f["could_conceal_target"]:
        return (False, "could_conceal_target is false", gap, pred)
    target = steps[-1]
    if f["stop"] != "complete":
        return (True, f'budget stop: {f["stop"]}', gap, pred)
    test_file = steps[0]["file"]
    if any("does not compile" in c and test_file in c for c in f["concealment"]):
        return (True, "the test file is reported as not compiling", gap, pred)
    listed = list(f["hidden_examples"]) + [p for e in f["excluded"] for p in e["plausible"]]
    if names(listed, target) or names(listed, gap):
        return (True, "the target or the gap function is listed as hidden or plausible", gap, pred)
    if any(e["from"] == pred["function"] for e in f["excluded"]) or any(
        f'{pred["function"]} →' in c for c in f["concealment"]
    ):
        return (True, "an excluded call from the predecessor is reported", gap, pred)
    return (False, "no report names the gap, its predecessor, the target, a budget stop or an uncompiled test file", gap, pred)


def analyze(fixtures, raw):
    v4 = {r["fixture"]: r for r in json.load(open(os.path.join(raw, "results-v4.json")))["fixtures"]}
    per = []
    for name in sorted(os.listdir(fixtures)):
        gt_path = os.path.join(fixtures, name, "GROUND_TRUTH.json")
        if not os.path.isfile(gt_path):
            continue
        gt = json.load(open(gt_path))
        s = v4.get(name, {"error": "missing from results-v4.json"})
        if "error" in s:
            per.append({"fixture": name, "error": s["error"]})
            continue
        out = json.load(open(os.path.join(raw, f"{name}.json")))
        tags = gt.get("tags", [])
        omissions = []
        for i, t in enumerate(s["targets"]):
            if t["rank"] is not None:
                continue
            paths = [p for p in gt.get("paths", []) if p.get("target") == i]
            if not paths:
                omissions.append({"target": t["target"], "reported": False, "reason": "no documented path"})
                continue
            for p in paths:
                ok, why, gap, pred = material_report(p, out)
                omissions.append(
                    {
                        "target": t["target"],
                        "reported": ok,
                        "reason": why,
                        "gap": gap and f'{gap["file"]}:{gap["function"]} (via {gap.get("via")})',
                        "predecessor": pred and f'{pred["file"]}:{pred["function"]}',
                    }
                )
        single = s["single_symbol"]
        panic_site = gt.get("panic_site")
        p6_violation = bool(
            single
            and panic_site
            and score_v4.matches(single, panic_site)
            and not any(score_v4.matches(single, t) for t in gt["true_targets"])
        )
        p2_ambiguous_violation = bool(single and ("ambiguous" in tags or gt.get("any_of")))
        per.append(
            {
                "fixture": name,
                "category": gt["category"],
                "tags": tags,
                "decision": s["decision"],
                "single": single,
                "ranks": [t["rank"] for t in s["targets"]],
                "material_omissions": omissions,
                "p6_violation": p6_violation,
                "p2_ambiguous_violation": p2_ambiguous_violation,
                "could_conceal": s["could_conceal"],
                "stop": s["stop"],
                "candidates": s["candidates"],
                "rank_ms": s["rank_ms"],
            }
        )
    return per


def ranking_metrics(ranks):
    n = len(ranks)
    found = [r for r in ranks if r is not None]
    if n == 0:
        return None
    return {
        "targets": n,
        "found": len(found),
        "top1": round(sum(r <= 1 for r in found) / n, 3),
        "top5": round(sum(r <= 5 for r in found) / n, 3),
        "top10": round(sum(r <= 10 for r in found) / n, 3),
        "mrr": round(sum(1 / r for r in found) / n, 3),
        "mean_rank_found": round(sum(found) / len(found), 1) if found else None,
    }


def decide(v4_criteria, per):
    ok = [p for p in per if "error" not in p]
    p1 = all(o["reported"] for p in ok for o in p["material_omissions"])
    p2 = v4_criteria["P2_no_false_uniqueness"] and not any(p["p2_ambiguous_violation"] for p in ok)
    p6 = not any(p["p6_violation"] for p in ok)
    by_cat = {}
    for p in ok:
        by_cat.setdefault(p["category"], []).extend(p["ranks"])
    p7 = [r for p in ok if "implicit_dispatch" in p["tags"] for r in p["ranks"]]
    result = {
        "P1_material_omissions_reported": p1,
        "P1_flag_only_scorer_v4": v4_criteria["P1_no_silent_miss"],
        "P2_no_false_uniqueness_incl_ambiguous": p2,
        "P3_no_false_impossibility": v4_criteria["P3_no_false_impossibility"],
        "P4_recall_deep_dispatch": v4_criteria["P4_recall_deep_dispatch"],
        "P4_pass": v4_criteria["P4_pass"],
        "P5_bounded_cost": v4_criteria["P5_bounded_cost"],
        "P6_producer_consumer_safety": p6,
        "P7_implicit_dispatch_recall": ranking_metrics(p7),
        "P8_ranking_by_category": {c: ranking_metrics(r) for c, r in sorted(by_cat.items())},
        "P8_ranking_all": ranking_metrics([r for p in ok for r in p["ranks"]]),
        "errored": [p["fixture"] for p in per if "error" in p] + v4_criteria["fixtures_errored"],
    }
    result["decision"] = (
        "PASS"
        if p1 and p2 and p6 and result["P3_no_false_impossibility"] and result["P4_pass"] and result["P5_bounded_cost"] and not result["errored"]
        else "FAIL"
    )
    return result


if __name__ == "__main__":
    fixtures, raw = sys.argv[1], sys.argv[2]
    v4 = json.load(open(os.path.join(raw, "results-v4.json")))
    per = analyze(fixtures, raw)
    result = {"criteria": decide(v4["criteria"], per), "fixtures": per}
    open(os.path.join(raw, "analysis-v5.json"), "w").write(json.dumps(result, indent=2))
    print(json.dumps(result["criteria"], indent=2))
