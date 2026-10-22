"""Rescoring of the frozen held-out raw outputs with ONE change to eval_v3.py: candidate ids are
split at the file's `.rs:` boundary instead of `rsplit(":", 1)`, which cut inside `Type::method`
and made every method target unmatchable. Criteria, thresholds and raw outputs are unchanged; no
ranking is rerun. eval_v3.py itself is left as it was and its results stay the designated output.

usage: python3 -I rescore_v3_idfix.py FIXTURES_DIR RAW_DIR
"""
import importlib.util, json, os, sys

here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("eval_v3", os.path.join(here, "eval_v3.py"))
ev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ev)


def matches(cid, t):
    rest = cid.split(":", 1)[1]
    path, qualified = rest.split(".rs:", 1)
    path += ".rs"
    return (path == t["file"] or path.endswith("/" + t["file"])) and qualified.split("#")[0] == t["function"]


ev.matches = matches
fixtures, raw = sys.argv[1], sys.argv[2]
results = []
for name in sorted(os.listdir(fixtures)):
    gt_path = os.path.join(fixtures, name, "GROUND_TRUTH.json")
    if os.path.isfile(gt_path):
        results.append(ev.score(name, json.load(open(gt_path)), json.load(open(os.path.join(raw, name + ".json")))))
summary = {"note": "id-parsing fix only; see docstring", "criteria": ev.criteria(results), "fixtures": results}
open(os.path.join(raw, "results-idfix.json"), "w").write(json.dumps(summary, indent=2))
print(json.dumps(summary["criteria"], indent=2))
for r in results:
    print(r["fixture"], r["category"], r["decision"], r["single_symbol"], [(t["target"].split(":")[-1], t["rank"]) for t in r["targets"]], "conceal=", r["could_conceal"], "cands=", r["candidates"], "ms=", r["rank_ms"])
