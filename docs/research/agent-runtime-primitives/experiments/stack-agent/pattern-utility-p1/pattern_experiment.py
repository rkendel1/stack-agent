#!/usr/bin/env python3
"""Pattern utility experiment harness (stdlib only).

  pattern_experiment.py validate-corpus   REPO WORK
  pattern_experiment.py select            REPO BIN OUT
  pattern_experiment.py trial             REPO BIN WORK OUT [--tasks a,b] [--conditions baseline,candidate] [--repeat N]

REPO is a clean checkout of the pinned commit with `stack-agent init` already run on it (the graph and
pattern report every selection and every pattern check refers to). BIN is a `stack-agent` binary.
Nothing here edits REPO: every trial works in a fresh copy.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.join(HERE, "corpus")
TASKS = json.load(open(os.path.join(CORPUS, "tasks.json")))["tasks"]
IGNORE = shutil.ignore_patterns(".git", "target", ".chip", "node_modules", ".stack-agent")
LOCAL = {"CHIP_PROVIDER": "openai-compatible", "CHIP_MODEL": os.environ.get("PU_MODEL", "google/gemma-4-26b-a4b-qat"),
         "CHIP_ENDPOINT": "http://localhost:1235/v1/chat/completions"}
# The benchmark's `local-object` cell: the canonical schema is harmful for this model (docs/product/agent-bench.md).
LOCAL_FLAGS = ["--provider-profile", "local", "--structured-output", "object"]
MAX_TURNS, MAX_EXECUTIONS, DEADLINE, CALL_TIMEOUT, MAX_OUT = 16, 12, 1500, 300, 8192
FOLLOW = {  # the repo's own marker for an unrelated path
    "prefixes": (".stack-agent/", ".chip/", "target/"),
}


def by_id(tid):
    return next(t for t in TASKS if t["id"] == tid)


def copy_repo(src, dst):
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst, ignore=IGNORE, symlinks=False)
    subprocess.run(["git", "init", "-q", "."], cwd=dst, check=True)
    subprocess.run(["git", "add", "-A"], cwd=dst, check=True, capture_output=True)
    subprocess.run(["git", "-c", "user.email=x@x", "-c", "user.name=x", "commit", "-q", "-m", "base"],
                   cwd=dst, check=True, capture_output=True)


def cargo(args, cwd, target, timeout=1800):
    env = dict(os.environ, CARGO_TARGET_DIR=target)
    t0 = time.perf_counter()
    try:
        p = subprocess.run(["cargo"] + args + ["--offline", "--locked"], cwd=cwd, env=env,
                           capture_output=True, text=True, timeout=timeout)
        out, code = p.stdout + p.stderr, p.returncode
    except subprocess.TimeoutExpired as e:
        out, code = ((e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")) + "TIMEOUT", -1
    return code, out, time.perf_counter() - t0


def totals(out):
    passed = failed = 0
    for m in re.finditer(r"test result: \w+\. (\d+) passed; (\d+) failed", out):
        passed += int(m.group(1))
        failed += int(m.group(2))
    return passed, failed


def run_test_file(trial, target, crate, name):
    code, out, _ = cargo(["test", "-p", crate, "--test", name], trial, target)
    p, f = totals(out)
    return {"compiled": "error[" not in out and "could not compile" not in out, "passed": p, "failed": f, "code": code}


def accept_test_file(task, trial, target):
    acc = task["accept"]
    dest_name = "acceptance_pu_" + acc["file"].replace(".rs", "")
    dest = os.path.join(trial, "crates", task["crate"], "tests", dest_name + ".rs")
    shutil.copy(os.path.join(CORPUS, "acceptance", acc["file"]), dest)
    r = run_test_file(trial, target, task["crate"], dest_name)
    os.remove(dest)
    r["ok"] = bool(r["compiled"] and r["failed"] == 0 and r["passed"] == acc["expect_passed"] and r["code"] == 0)
    return r


MUTANTS = [  # (name, file, old, new): a correct set of tests must fail under each
    ("intent_key ignores the attempt", "crates/chip-agent-state/src/lib.rs",
     'let key = format!("a{attempt}.{execution_id}");', 'let key = format!("a0.{execution_id}");'),
    ("intent_key ignores the execution id", "crates/chip-agent-state/src/lib.rs",
     'let key = format!("a{attempt}.{execution_id}");', 'let key = format!("a{attempt}.x");'),
    ("action_key ignores the execution id", "crates/chip-agent-state/src/lib.rs",
     'for part in ["chip.action-key.v1", task_id, execution_id] {', 'for part in ["chip.action-key.v1", task_id, "x"] {'),
    ("action_key ignores the task", "crates/chip-agent-state/src/lib.rs",
     'for part in ["chip.action-key.v1", task_id, execution_id] {', 'for part in ["chip.action-key.v1", "t", execution_id] {'),
]


def new_test_files(task, trial):
    d = os.path.join(trial, "crates", task["crate"], "tests")
    base = subprocess.run(["git", "ls-files", "crates/%s/tests" % task["crate"]], cwd=trial,
                          capture_output=True, text=True).stdout.split()
    return sorted(f[:-3] for f in os.listdir(d)
                  if f.endswith(".rs") and "crates/%s/tests/%s" % (task["crate"], f) not in base)


def accept_tests_with_mutants(task, trial, target):
    files = new_test_files(task, trial)
    res = {"files": files, "ok": False, "tests": 0, "failed": 0, "mutants_killed": [], "mutants_survived": []}
    if not files:
        res["why"] = "no new test file under crates/%s/tests/" % task["crate"]
        return res
    for f in files:
        r = run_test_file(trial, target, task["crate"], f)
        res["tests"] += r["passed"]
        res["failed"] += r["failed"]
        if not r["compiled"]:
            res["why"] = "a new test file does not compile"
            res["failed"] += 1
    if res["failed"] or res["tests"] < task["accept"]["min_tests"]:
        res.setdefault("why", "tests fail or fewer than %d" % task["accept"]["min_tests"])
        return res
    for name, rel, old, new in MUTANTS:
        p = os.path.join(trial, rel)
        original = open(p).read()
        if old not in original:
            res["mutants_survived"].append(name + " (not applicable)")
            continue
        open(p, "w").write(original.replace(old, new, 1))
        try:
            killed = False
            for f in files:
                r = run_test_file(trial, target, task["crate"], f)
                if r["failed"] > 0 or not r["compiled"]:
                    killed = True
        finally:
            open(p, "w").write(original)
        (res["mutants_killed"] if killed else res["mutants_survived"]).append(name)
    res["ok"] = not res["mutants_survived"]
    return res


def acceptance(task, trial, target):
    if task["accept"]["kind"] == "test":
        return accept_test_file(task, trial, target)
    return accept_tests_with_mutants(task, trial, target)


def regression(task, trial, target):
    """The crate's own tests, without the hidden acceptance file."""
    code, out, secs = cargo(["test", "-p", task["crate"], "--no-fail-fast"], trial, target)
    p, f = totals(out)
    compiled = "could not compile" not in out
    return {"compiled": compiled, "passed": p, "failed": f, "ok": compiled and f == 0, "seconds": round(secs, 1)}


def visible_check(task, trial, target):
    """What the model's `pax.test` runs: the crate compiles, tests included."""
    return cargo(["check", "-p", task["crate"], "--all-targets"], trial, target)


def changed_paths(trial):
    out = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=trial,
                         capture_output=True, text=True).stdout.splitlines()
    paths = []
    for line in out:
        p = line[3:].strip().strip('"')
        if p.startswith(FOLLOW["prefixes"]):
            continue
        paths.append(p)
    return sorted(paths)


def unrelated(task, paths):
    return [p for p in paths if not any(p == a or (a.endswith("/") and p.startswith(a)) for a in task["allowed_paths"])]


def cmd_validate_corpus(repo, work):
    target = os.path.join(work, "target")
    out = {}
    only = os.environ.get("PU_ONLY")
    for task in TASKS:
        if only and task["id"].split("-")[0] not in only.split(","):
            continue
        # Every existing file a task edits must fit through `project.write` (32 KiB).
        sizes = {a: os.path.getsize(os.path.join(repo, a)) for a in task["allowed_paths"]
                 if os.path.isfile(os.path.join(repo, a))}
        too_big = {a: n for a, n in sizes.items() if n > 32 * 1024}
        assert not too_big, "%s edits files larger than project.write allows: %s" % (task["id"], too_big)
        base = os.path.join(work, task["id"] + "-base")
        ref = os.path.join(work, task["id"] + "-ref")
        copy_repo(repo, base)
        copy_repo(repo, ref)
        before = acceptance(task, base, target)
        subprocess.run([sys.executable, os.path.join(CORPUS, "reference", "apply.py"), task["id"].split("-")[0], ref], check=True)
        after = acceptance(task, ref, target)
        reg = regression(task, ref, target)
        out[task["id"]] = {"existing_files_edited_and_sizes": sizes, "fails_on_unmodified_repo": not before["ok"], "passes_on_reference": after["ok"],
                           "regression_on_reference": reg, "unmodified": before, "reference": after}
        print(task["id"], "unmodified fails:", not before["ok"], "| reference passes:", after["ok"],
              "| regression ok:", reg["ok"], reg["passed"], "passed", file=sys.stderr)
        shutil.rmtree(base, ignore_errors=True)
        shutil.rmtree(ref, ignore_errors=True)
    json.dump(out, open(os.path.join(work, "validate-corpus.json"), "w"), indent=1)


GOAL_LIMIT = 2000  # `stack-agent work` refuses a longer goal (MAX_GOAL_BYTES); the block rides in the goal


def budget_for(task):
    """Bytes left for the pattern block in a candidate goal: the limit, less the goal and the blank line."""
    return GOAL_LIMIT - len(task["goal"].encode()) - 2


def run_select(bin_path, root, task_rep, text=False, extra=()):
    tf = os.path.join(root, ".chip", "pu-task.json")
    json.dump(task_rep, open(tf, "w"))
    args = [bin_path, "graph", "patterns", "select", "--task", tf, "--root", root] + list(extra)
    p = subprocess.run(args + ([] if text else ["--json"]), capture_output=True, text=True)
    os.remove(tf)
    return p.returncode, p.stdout


def render_for(bin_path, repo, task):
    """The block a candidate goal carries: compact, within what the goal channel has left."""
    code, text = run_select(bin_path, repo, task["selector_task"], text=True,
                            extra=["--max-bytes", str(budget_for(task)), "--max-examples", "1"])
    return text.rstrip("\n")


def matches(entry, p):
    if p["category"] != entry["category"] or p["family"] != entry["family"]:
        return False
    return entry["signature"] is None or p["signature"] == entry["signature"]


def evaluate_selection(task, sel):
    pats = sel["patterns"]
    expected = task["expected_relevant"]
    found = []
    for e in expected:
        hit = [p for p in pats if matches(e, p)]
        if e["must_be"] == "selected_if_any_test_mentions_the_symbols":
            found.append({"expected": e, "found": any(any(r["signal"] == "verification" for r in p["reasons"]) for p in hit)})
        else:
            found.append({"expected": e, "found": bool(hit)})
    unexpected = [p for p in pats if not any(matches(e, p) for e in expected)]
    return {"status": sel["status"], "expected_found": found,
            "all_expected_found": all(f["found"] for f in found),
            "no_pattern_expected_and_none_offered": (not expected) and not pats,
            "patterns_selected": len(pats), "unexpected_selected": len(unexpected),
            "unexpected": [{"family": p["family"], "signature": p["signature"], "why": [r["signal"] for r in p["reasons"]]} for p in unexpected]}


def cmd_select(repo, bin_path, out):
    os.makedirs(out, exist_ok=True)
    result = {"tasks": {}, "failure_cases": {}}
    for task in TASKS:
        code, js = run_select(bin_path, repo, task["selector_task"])
        sel = json.loads(js)
        text = render_for(bin_path, repo, task)
        code, full_text = run_select(bin_path, repo, task["selector_task"], text=True)
        ev = evaluate_selection(task, sel)
        ev["full_rendering_bytes"] = len(full_text.encode())
        ev["goal_bytes"] = len(task["goal"].encode())
        ev["goal_channel_budget"] = budget_for(task)
        ev["patterns_in_goal_channel_block"] = text.count("\n- ") + (1 if text.startswith("- ") else 0)
        ev.update({"rendered_bytes": len(text.encode()), "approx_tokens": len(text.encode()) // 4,
                   "coverage_complete": sel["coverage_complete"], "warnings": sel["warnings"],
                   "unknown_references": sel["unknown_references"], "explanation": sel["explanation"]})
        ev["selection"] = sel
        ev["rendered"] = text
        result["tasks"][task["id"]] = ev
    # Failure cases that need a variation of the task or of the repository.
    fc = result["failure_cases"]
    # 1. A symbol that does not exist yet and whose name resembles one that does.
    code, js = run_select(bin_path, repo, {"id": "fc-similar-name", "symbols": ["symbol:crates/chip-core/src/lib.rs:Agent::execute_capability_limited"]})
    fc["similar_name_does_not_match"] = json.loads(js)
    # 2. Conflicting examples: a family with two real shapes.
    code, js = run_select(bin_path, repo, {"id": "fc-conflict", "families": ["ObservationPredicate"]})
    fc["conflicting_examples"] = json.loads(js)
    # 3. Nothing named.
    code, js = run_select(bin_path, repo, {"id": "fc-vague"})
    fc["task_that_names_nothing"] = json.loads(js)
    # 4. A stale report: the same task after one of its files changed.
    stale = os.path.join(out, "stale-copy")
    shutil.rmtree(stale, ignore_errors=True)
    shutil.copytree(repo, stale, ignore=shutil.ignore_patterns(".git", "target", "node_modules"), symlinks=False)
    with open(os.path.join(stale, "crates/chip-graph/src/store.rs"), "a") as f:
        f.write("\n// changed after the report was built\n")
    code, js = run_select(bin_path, stale, by_id("t1-extend-error")["selector_task"])
    fc["stale_report"] = dict(json.loads(js), exit_code=code)
    shutil.rmtree(stale, ignore_errors=True)
    json.dump(result, open(os.path.join(out, "selection-results.json"), "w"), indent=1)
    for tid, ev in result["tasks"].items():
        print(tid, ev["status"], "expected found:", ev["all_expected_found"], "| selected", ev["patterns_selected"],
              "unexpected", ev["unexpected_selected"], "| ~tokens", ev["approx_tokens"], file=sys.stderr)


PAX_STAND_IN = r"""#!/usr/bin/env python3
# The verification the model's `pax.test` runs in these trials: the crate compiles, tests included.
# A stand-in for PAX, the same in both conditions (see README: unavoidable difference).
import json, os, subprocess, sys
argv = sys.argv[1:]
if argv == ["--version"]:
    print("pax 0.4.1")
    sys.exit(0)
root = argv[argv.index("--dir") + 1] if "--dir" in argv else os.getcwd()
env = dict(os.environ, CARGO_TARGET_DIR=%(target)r, PATH=%(path)r, HOME=%(home)r)
for k in ("CARGO_HOME", "RUSTUP_HOME"):
    if %(rust_env)r.get(k):
        env[k] = %(rust_env)r[k]
p = subprocess.run(["cargo", "check", "-p", %(crate)r, "--all-targets", "--offline", "--locked", "--message-format", "short"],
                   cwd=root, env=env, capture_output=True, text=True)
lines = [l for l in p.stderr.splitlines() if "error" in l or "warning: unused" in l][:40]
sys.stderr.write("\n".join(lines)[:6000] + "\n")
ok = p.returncode == 0
print(json.dumps({"schema": "pax.execution-result.v1", "operation": "test", "status": "passed" if ok else "failed",
                  "reason": "tests-passed" if ok else "tests-failed", "tool": "cargo", "exit_code": 0 if ok else 101,
                  "tests": {"passed": 1 if ok else 0, "failed": 0 if ok else 1, "ignored": 0, "measured": 0}},
                 separators=(",", ":")))
"""


def install_pax(dirpath, crate, target):
    os.makedirs(dirpath, exist_ok=True)
    path = os.path.join(dirpath, "pax")
    open(path, "w").write(PAX_STAND_IN % {
        "target": target, "crate": crate, "path": os.environ["PATH"], "home": os.environ["HOME"],
        "rust_env": {k: os.environ.get(k) for k in ("CARGO_HOME", "RUSTUP_HOME")}})
    os.chmod(path, 0o755)


def load_report(root):
    import glob
    latest = open(os.path.join(root, ".chip", "graph", "patterns-latest")).read().strip()
    return json.load(open(os.path.join(root, ".chip", "graph", latest + ".json")))


NAME_KEY = {"t2-new-in-uniform-family": "CompleteAfter", "t3-common-shape-is-wrong-here": "EchoBackend",
            "t4-ordering": "execute_capability_limited", "t6-novel": "BoundedLog"}


def pattern_check(task, bin_path, trial, base):
    """Did the result follow, or depart from, the relevant observed pattern? Measured by P0's own
    analyzer on the trial's files, not by reading the code."""
    chk = task["pattern_check"]
    subprocess.run([bin_path, "init", "--root", trial], capture_output=True)
    try:
        now = load_report(trial)
    except Exception as e:  # the trial may not parse at all
        return {"result": "unmeasurable", "why": "the analyzer could not run on the trial: %s" % e}
    base_ids = {o["id"] for o in base["observations"]}
    new = [o for o in now["observations"] if o["id"] not in base_ids]
    out = {"new_observations": len(new)}
    kind = chk["kind"]
    if kind == "signature_unchanged":
        mine = [o for o in now["observations"] if o["category"] == chk["category"] and o["name"] == chk["name"]]
        out["result"] = "unchanged" if mine and mine[0]["signature"] == chk["signature"] else "changed_or_missing"
        out["signature"] = mine[0]["signature"] if mine else None
    elif kind == "new_observation":
        key = NAME_KEY[task["id"]]
        mine = [o for o in new if o["category"] == chk["category"] and o["family"] == chk["family"] and key in o["name"]]
        out["observations"] = [{"name": o["name"], "signature": o["signature"], "file": o["file"]} for o in mine]
        if not mine:
            out["result"] = "no_observation"
        elif any(o["signature"] == chk["followed_signature"] for o in mine):
            out["result"] = "followed_relevant_pattern"
        elif any(o["signature"] == chk.get("counter_signature") for o in mine):
            out["result"] = "followed_counterexample"
        else:
            out["result"] = "other_shape"
    elif kind == "new_tests":
        tests = [o for o in new if o["category"] == "test_shape" and o["file"].startswith("crates/chip-agent-state/tests/")]
        out["tests"] = [{"name": o["name"], "signature": o["signature"], "mentions": o["mentions"]} for o in tests]
        placed = all("integration" in o["signature"] for o in tests) and bool(tests)
        mentioned = all(any(m in o["mentions"] for o in tests) for m in chk["mentions"]) if tests else False
        out["result"] = "followed_test_organization" if placed and mentioned else ("placed_but_not_mentioning" if placed else "not_in_tests_dir")
    else:
        out["result"] = "no_pattern_expected"
        out["observations"] = [{"name": o["name"], "category": o["category"], "signature": o["signature"]}
                               for o in new if key_in(o, NAME_KEY.get(task["id"]))]
    return out


def key_in(o, key):
    return bool(key) and key in o["name"]


def run_trial(task, condition, repo, bin_path, work, target, base_report, block, rep):
    tdir = os.path.join(work, "trials", "%s-%s-%d" % (task["id"], condition, rep))
    trial = os.path.join(tdir, "repo")
    shutil.rmtree(tdir, ignore_errors=True)
    os.makedirs(tdir)
    copy_repo(repo, trial)
    pax_dir = os.path.join(tdir, "bin")
    install_pax(pax_dir, task["crate"], target)
    goal = task["goal"] if condition == "baseline" else task["goal"] + "\n\n" + block
    env = {k: os.environ[k] for k in ("HOME", "RUSTUP_HOME", "CARGO_HOME", "TMPDIR", "LANG", "USER") if k in os.environ}
    env["PATH"] = pax_dir + os.pathsep + os.environ["PATH"]
    env.update(LOCAL)
    env["STACK_AGENT_DATA_DIR"] = os.path.join(tdir, "data")
    args = [bin_path, "work", goal, "--kind", "change", "--max-turns", str(MAX_TURNS), "--max-executions", str(MAX_EXECUTIONS),
            "--deadline-secs", str(DEADLINE), "--call-timeout-secs", str(CALL_TIMEOUT), "--max-output-tokens", str(MAX_OUT),
            "--print-reply", "--json"] + LOCAL_FLAGS
    t0 = time.perf_counter()
    try:
        p = subprocess.run(args, cwd=trial, env=env, capture_output=True, text=True, timeout=DEADLINE + 300, stdin=subprocess.DEVNULL)
        stdout, stderr, code = p.stdout, p.stderr, p.returncode
    except subprocess.TimeoutExpired as e:
        stdout, stderr, code = "", "TIMEOUT", -1
    wall = time.perf_counter() - t0
    report = None
    for line in reversed(stdout.strip().splitlines()):
        try:
            report = json.loads(line)
            break
        except ValueError:
            continue
    open(os.path.join(tdir, "stderr.txt"), "w").write(stderr[-200000:])
    r = report or {}
    m = r.get("measurement") or {}
    rec = {"task": task["id"], "condition": condition, "repeat": rep, "exit": code, "wall_seconds": round(wall, 1),
           "terminal_state": r.get("terminal_state"), "outcome_reason": (r.get("outcome_reason") or "")[:300],
           "model_calls": m.get("model_calls"), "model_tokens": m.get("model_tokens"),
           "model_latency_ms": m.get("model_latency_ms"), "turns": m.get("turns"),
           "executions_by_capability": r.get("executions_by_capability"),
           "invalid_decisions": r.get("invalid_decisions"), "invalid_inputs": r.get("invalid_inputs"),
           "pattern_block_bytes": len(block.encode()) if condition == "candidate" else 0,
           "goal_bytes": len(goal.encode()), "human_correction": 0}
    paths = changed_paths(trial)
    rec["changed_paths"] = paths
    rec["unrelated_changes"] = unrelated(task, paths)
    rec["acceptance"] = acceptance(task, trial, target)
    rec["regression"] = regression(task, trial, target)
    rec["pattern"] = pattern_check(task, bin_path, trial, base_report)
    json.dump(rec, open(os.path.join(tdir, "record.json"), "w"), indent=1)
    return rec


def cmd_trial(repo, bin_path, work, out, args):
    opt = lambda name, default: (args[args.index(name) + 1] if name in args else default)
    tasks = opt("--tasks", None)
    tasks = [t for t in TASKS if not tasks or t["id"].split("-")[0] in tasks.split(",") or t["id"] in tasks.split(",")]
    conditions = opt("--conditions", "baseline,candidate").split(",")
    repeats = int(opt("--repeat", "1"))
    os.makedirs(out, exist_ok=True)
    target = os.path.join(work, "target")
    base = load_report(repo)
    results = []
    for rep in range(1, repeats + 1):
        for task in tasks:
            block = render_for(bin_path, repo, task)
            for cond in conditions:
                rec = run_trial(task, cond, repo, bin_path, work, target, base, block, rep)
                results.append(rec)
                json.dump(results, open(os.path.join(out, "trial-results.json"), "w"), indent=1)
                print(task["id"], cond, rec["terminal_state"], "calls", rec["model_calls"], "accept", rec["acceptance"]["ok"],
                      "regr", rec["regression"]["ok"], "pattern", rec["pattern"].get("result"), "%.0fs" % rec["wall_seconds"], file=sys.stderr)


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "trial":
        cmd_trial(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6:])
    if cmd == "select":
        cmd_select(*sys.argv[2:5])
    if cmd == "validate-corpus":
        cmd_validate_corpus(*sys.argv[2:4])
