"""Held-out evaluation of targeting v3: run the frozen rank_target on each fixture's scratch copy
and score it against GROUND_TRUTH.json under the pre-registered criteria P1-P5.

usage: python3 -I eval_v3.py FIXTURES_DIR OUT_DIR
"""
import json
import os
import shutil
import subprocess
import sys

S = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(S, "rank_target.frozen")


def run(fixtures, out):
    os.makedirs(out, exist_ok=True)
    work = os.path.join(S, "heldout-eval")
    target = os.path.join(S, "heldout-eval-target")
    results = []
    for name in sorted(os.listdir(fixtures)):
        src = os.path.join(fixtures, name)
        gt_path = os.path.join(src, "GROUND_TRUTH.json")
        if not os.path.isfile(gt_path):
            continue
        gt = json.load(open(gt_path))
        copy = os.path.join(work, name)
        shutil.rmtree(copy, ignore_errors=True)
        shutil.copytree(src, copy, ignore=shutil.ignore_patterns("target", "solution.patch", "GROUND_TRUTH.json"))
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR", "LANG")}
        env["CARGO_TARGET_DIR"] = target
        p = subprocess.run([BIN, copy] + gt["acceptance"], capture_output=True, text=True, env=env)
        raw = os.path.join(out, f"{name}.json")
        open(raw, "w").write(p.stdout)
        if p.returncode != 0 or not p.stdout.strip():
            results.append({"fixture": name, "error": p.stderr[-2000:]})
            continue
        results.append(score(name, gt, json.loads(p.stdout)))
    return results


def matches(cid, t):
    # symbol:<path>:<qualified>; the ground truth names the file relative to the fixture root.
    _, rest = cid.split(":", 1)
    path, qualified = rest.rsplit(":", 1)
    q = qualified.split("#")[0]
    return (path == t["file"] or path.endswith("/" + t["file"])) and q == t["function"]


def score(name, gt, out):
    r = out["ranking"]
    cands = r["top"]
    decision = out["decision"]
    targets = []
    for t in gt["true_targets"]:
        pos = [i + 1 for i, c in enumerate(cands) if matches(c["id"], t)]
        targets.append({"target": f'{t["file"]}:{t["function"]}', "rank": pos[0] if pos else None})
    single = decision["kind"] == "single"
    single_ok = single and any(matches(decision["symbols"], t) for t in gt["true_targets"])
    f = r["frontier"]
    return {
        "fixture": name,
        "category": gt["category"],
        "fix_kind": gt["fix_kind"],
        "decision": decision["kind"],
        "single_symbol": decision["symbols"] if single else None,
        "single_correct": single_ok if single else None,
        "shape": r["shape"]["expressible"],
        "shape_reasons": r["shape"]["reasons"],
        "targets": targets,
        "candidates": r["total_candidates"],
        "could_conceal": f["could_conceal_target"],
        "concealment": f["concealment"],
        "stop": f["stop"],
        "explored": f["explored"],
        "edges": f["edges"],
        "max_depth": f["max_depth_reached"],
        "cut_off": f["cut_off"],
        "excluded": f["excluded_counts"],
        "unresolved_dispatch": f["unresolved_dispatch"],
        "rank_ms": out["cost"]["rank_ms"],
        "index_ms": out["cost"]["index_ms"],
        "timed_out": out["acceptance"]["timed_out"],
        "diagnostics": [(d["kind"], d["path"], d["line"]) for d in out["acceptance"]["diagnostics"] if d["kind"] != "failed_test"],
    }


def criteria(results):
    ok = [r for r in results if "error" not in r]
    p1 = all(t["rank"] is not None or r["could_conceal"] for r in ok for t in r["targets"])
    p2 = all(
        (r["decision"] != "single") or (r["single_correct"] and r["fix_kind"] == "single_body")
        for r in ok
    )
    p3 = all(r["decision"] != "not_expressible" for r in ok if r["category"] in ("deep", "dispatch"))
    body = [t for r in ok if r["category"] in ("deep", "dispatch") for t in r["targets"]]
    recall = sum(t["rank"] is not None for t in body) / len(body) if body else 0.0
    p4 = recall >= 0.8
    p5 = all(r["rank_ms"] <= 10_000 for r in ok) and all(
        (r["stop"] == "complete") or r["could_conceal"] for r in ok
    )
    md = [r for r in ok if r["category"] in ("multi", "declaration")]
    all_t = [t for r in ok for t in r["targets"]]
    found = [t["rank"] for t in all_t if t["rank"] is not None]
    return {
        "fixtures_scored": len(ok),
        "fixtures_errored": [r["fixture"] for r in results if "error" in r],
        "P1_no_silent_miss": p1,
        "P2_no_false_uniqueness": p2,
        "P3_no_false_impossibility": p3,
        "P4_recall_deep_dispatch": round(recall, 3),
        "P4_pass": p4,
        "P5_bounded_cost": p5,
        "all_pass": p1 and p2 and p3 and p4 and p5 and not any("error" in r for r in results),
        "reported": {
            "multi_declaration_not_expressible": f'{sum(r["decision"] == "not_expressible" for r in md)}/{len(md)}',
            "recall_all_targets": f"{len(found)}/{len(all_t)}",
            "top8_all_targets": f"{sum(x <= 8 for x in found)}/{len(all_t)}",
            "mean_rank_found": round(sum(found) / len(found), 1) if found else None,
            "max_rank_ms": max((r["rank_ms"] for r in ok), default=None),
            "timeouts": sum(r["timed_out"] for r in ok),
        },
    }


if __name__ == "__main__":
    fixtures, out = sys.argv[1], sys.argv[2]
    results = run(fixtures, out)
    summary = {"criteria": criteria(results), "fixtures": results}
    open(os.path.join(out, "results.json"), "w").write(json.dumps(summary, indent=2))
    print(json.dumps(summary["criteria"], indent=2))
