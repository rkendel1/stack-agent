"""Scorer v4 for symbol-loop targeting evaluations.

Successor to the v3 scorer (`2026-10-22-targeting-v3-held-out/scorer/eval_v3.py`), which is kept
unchanged with its recorded results. v4 changes only how symbol ids are parsed and matched; the
metrics and the P1-P5 criteria are the ones `2026-10-22-targeting-v3-held-out/PREREGISTRATION.md`
defines. Semantics are in README.md; regression tests are in test_score_v4.py.

usage:
  python3 -I score_v4.py run BINARY FIXTURES_DIR OUT_DIR   # rank each fixture, then score
  python3 -I score_v4.py rescore FIXTURES_DIR RAW_DIR      # score existing raw outputs
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

# symbol:<repository-relative .rs path>:<qualified name>[#n]
# The path is the shortest prefix ending in `.rs` after which the rest is a qualified name: segments
# separated by `::`, none empty and none containing `:`, optionally ending in `#<digits>` (the
# index's suffix for a repeated qualified name in one file).
_ID = re.compile(r"^symbol:(?P<path>.+?\.rs):(?P<qualified>[^:]+(?:::[^:]+)*)$")
_DUP = re.compile(r"^(?P<base>.+)#(?P<n>[0-9]+)$")


class BadId(ValueError):
    pass


def parse_symbol_id(cid):
    """(path, qualified, duplicate index or None) of a chip-symbol-loop symbol id."""
    m = _ID.match(cid)
    if not m:
        raise BadId(cid)
    qualified = m.group("qualified")
    dup = None
    d = _DUP.match(qualified)
    if d:
        qualified, dup = d.group("base"), int(d.group("n"))
    if any(seg.strip() == "" for seg in qualified.split("::")):
        raise BadId(cid)
    return m.group("path"), qualified, dup


def _norm_function(name):
    # Whitespace never belongs to a Rust path; `Type :: method` and `Type::method` are one name.
    return re.sub(r"\s+", "", name)


def _norm_path(path):
    path = path.replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path


def matches(cid, target):
    """Whether candidate id `cid` names the ground-truth function {file, function}.

    The file matches when the paths are equal or the id's path ends with `/<file>` (an id may be
    relative to a parent directory of the fixture root). The function matches when the qualified
    name, without a `#n` duplicate suffix, equals it exactly: `Type::method`, `outer::inner::f` and
    `f` are compared whole, never by their last segment.
    """
    path, qualified, _ = parse_symbol_id(cid)
    path, file = _norm_path(path), _norm_path(target["file"])
    return (path == file or path.endswith("/" + file)) and _norm_function(qualified) == _norm_function(
        target["function"]
    )


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
        "diagnostics": [
            (d["kind"], d["path"], d["line"])
            for d in out["acceptance"]["diagnostics"]
            if d["kind"] not in ("failed_test", "passed_test")
        ],
    }


def criteria(results):
    """P1-P5 exactly as PREREGISTRATION.md (v3) defines them."""
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


def _fixtures(fixtures):
    for name in sorted(os.listdir(fixtures)):
        gt = os.path.join(fixtures, name, "GROUND_TRUTH.json")
        if os.path.isfile(gt):
            yield name, json.load(open(gt))


def enclosing_manifest(directory):
    """The first Cargo.toml above `directory`, if any: a fixture copied below one is built as part
    of that workspace (or refused by cargo), which is an infrastructure fault, not a result."""
    d = os.path.dirname(os.path.abspath(directory))
    while True:
        m = os.path.join(d, "Cargo.toml")
        if os.path.isfile(m):
            return m
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def run(binary, fixtures, out, work_root=None):
    os.makedirs(out, exist_ok=True)
    # Fixture copies go in a fresh temporary directory (TMPDIR, or work_root), never under OUT_DIR,
    # which may be inside a repository.
    work = tempfile.mkdtemp(prefix="score-v4-", dir=work_root)
    manifest = enclosing_manifest(os.path.join(work, "fixture"))
    if manifest:
        shutil.rmtree(work, ignore_errors=True)
        raise SystemExit(f"refusing to run: fixture copies under {work} would be inside the Cargo workspace of {manifest}")
    target = os.path.join(work, "_target")
    results = []
    for name, gt in _fixtures(fixtures):
        copy = os.path.join(work, name)
        shutil.rmtree(copy, ignore_errors=True)
        shutil.copytree(
            os.path.join(fixtures, name),
            copy,
            ignore=shutil.ignore_patterns("target", "solution.patch", "GROUND_TRUTH.json"),
        )
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR", "LANG")}
        env["CARGO_TARGET_DIR"] = target
        p = subprocess.run([binary, copy] + gt["acceptance"], capture_output=True, text=True, env=env)
        open(os.path.join(out, f"{name}.json"), "w").write(p.stdout)
        if p.returncode != 0 or not p.stdout.strip():
            results.append({"fixture": name, "error": p.stderr[-2000:]})
            continue
        results.append(score(name, gt, json.loads(p.stdout)))
    shutil.rmtree(work, ignore_errors=True)
    shutil.rmtree(target, ignore_errors=True)
    return results


def rescore(fixtures, raw):
    return [score(name, gt, json.load(open(os.path.join(raw, f"{name}.json")))) for name, gt in _fixtures(fixtures)]


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "run" and len(sys.argv) == 5:
        results, out = run(sys.argv[2], sys.argv[3], sys.argv[4]), sys.argv[4]
        name = "results-v4.json"
    elif mode == "rescore" and len(sys.argv) == 4:
        results, out = rescore(sys.argv[2], sys.argv[3]), sys.argv[3]
        name = "results-v4-rescore.json"
    else:
        sys.exit(__doc__)
    summary = {"scorer": "v4", "criteria": criteria(results), "fixtures": results}
    open(os.path.join(out, name), "w").write(json.dumps(summary, indent=2))
    print(json.dumps(summary["criteria"], indent=2))
