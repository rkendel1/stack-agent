#!/usr/bin/env python3
"""Feasibility-baseline trial harness (see PREREGISTRATION.md). Local only; no hosted model.

usage: harness.py --bin STACK_AGENT --cell A|B --task F1..F5 --trial N --out DIR [--upstream URL]
Writes DIR/<cell>-<task>-<trial>.json (one trace, one final category).
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
MODEL = "google/gemma-4-26b-a4b-qat"
CELLS = {"A": 2048, "B": 8192}
KIND = {"F1": "inspect", "F2": "inspect", "F3": "change", "F4": "change", "F5": "change"}
GOAL = {
    "F1": "List the files in the src directory of this project.",
    "F2": "Which function in src/math.rs multiplies its argument by three? Answer with the function name, citing the file you read.",
    "F3": 'In src/greeting.rs change the GREETING constant from "hello" to "welcome". Change nothing else.',
    "F4": "In src/math.rs add a public function double that takes a u64 and returns it multiplied by two as a u64. Do not change add or triple.",
    "F5": "The tests in tests/percent.rs describe the intended behaviour of clamp_percent in src/percent.rs. Make the project's tests pass without editing any test file.",
}
ONLY = {"F3": {"src/greeting.rs"}, "F4": {"src/math.rs"}, "F5": {"src/percent.rs"}}
CALL_TIMEOUT, DEADLINE = 240, 900


def sh(cmd, cwd, env=None, timeout=None):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout, env=env)


def cargo_test(d, target):
    r = sh(["cargo", "test", "--offline", "--quiet"], d, {**os.environ, "CARGO_TARGET_DIR": target}, 600)
    return r.returncode == 0


def changed_paths(d):
    out = sh(["git", "status", "--porcelain", "--untracked-files=all"], d).stdout
    return {l[3:].strip() for l in out.splitlines() if l.strip()}


def read(p):
    with open(p, "rb") as f:
        return f.read()


def accept(task, d, report, replies, target):
    """Deterministic acceptance. Returns (passed, detail)."""
    done = "complet" in str(report.get("terminal_state", "")).lower()
    if task == "F1":
        ok = False
        for r in replies:
            if r.get("decision") != "request_capability":
                continue
            p = (r.get("inputs") or {}).get("path", "")
            if (r.get("capability") == "project.list" and p.rstrip("/") == "src") or (
                r.get("capability") == "project.read" and p.startswith("src/")
            ):
                ok = True
        ok = ok and report.get("failed_observations", 0) == 0
        return done and ok, {"relevant_request": ok, "completed": done}
    if task == "F2":
        ans = (report.get("answer") or "") if isinstance(report.get("answer"), str) else json.dumps(report.get("answer"))
        ok = "triple" in ans and "src/math.rs" in ans and not changed_paths(d)
        return done and ok, {"answer_ok": ok, "completed": done}
    only = ONLY[task]
    ch = changed_paths(d)
    detail = {"changed": sorted(ch), "completed": done}
    if ch != only:
        detail["reason"] = "changed paths differ from the permitted set"
        return False, detail
    if task == "F3":
        same = read(os.path.join(d, "src/greeting.rs")) == read(os.path.join(HERE, "reference/f3/greeting.rs"))
        detail["exact_match"] = same
        return same and cargo_test(d, target), detail
    if task == "F4":
        src = read(os.path.join(d, "src/math.rs")).decode("utf-8", "replace")
        base = read(os.path.join(HERE, "fixture/src/math.rs")).decode()
        keep = all(b in src for b in (base.split("\n\n")[0], base.split("\n\n")[1].rstrip("\n")))
        detail["add_triple_unchanged"] = keep
        shutil.copy(os.path.join(HERE, "acceptance/f4.rs"), os.path.join(d, "tests/f4.rs"))
        return keep and cargo_test(d, target), detail
    if task == "F5":
        same = read(os.path.join(d, "tests/percent.rs")) == read(os.path.join(HERE, "overlays/f5/tests/percent.rs"))
        detail["tests_unmodified"] = same
        shutil.copy(os.path.join(HERE, "acceptance/f5.rs"), os.path.join(d, "tests/f5.rs"))
        return same and cargo_test(d, target), detail


def classify(task, accepted, report, replies, d, target, timed_out):
    """First match in PREREGISTRATION §6."""
    if accepted:
        return "accepted"
    reason = str(report.get("outcome_reason") or "")
    fails = report.get("model_failures") or []
    if reason.startswith("stuck: "):
        return "stuck_guard_terminated"
    if any(f.get("kind") == "length_limited" for f in fails):
        return "length_limited"
    if report.get("invalid_decisions", 0) or report.get("invalid_inputs", 0):
        return "invalid_capability_request"
    if report.get("failed_observations", 0):
        last = [r for r in replies if r.get("decision") == "request_capability"]
        cap = (last[-1].get("capability") if last else "") or ""
        if cap == "project.write":
            return "write_failure"
        if cap in ("project.read", "project.list"):
            return "read_failure"
    if report.get("capability_requests", 0) == 0 and not report.get("invalid_decisions", 0):
        return "capability_discovery_failure"
    if KIND[task] == "change":
        if report.get("writes", 0) and not cargo_test(d, target):
            return "compile_or_test_failure"
        if not report.get("writes", 0):
            return "implementation_incomplete"
    elif not report.get("answer"):
        return "implementation_incomplete"
    if "complet" in str(report.get("terminal_state", "")).lower():
        return "acceptance_failure"
    return "unknown"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--cell", required=True, choices=CELLS)
    ap.add_argument("--task", required=True, choices=KIND)
    ap.add_argument("--trial", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--upstream", default="http://localhost:1235")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    tid = f"{a.cell}-{a.task}-{a.trial}"
    work = tempfile.mkdtemp(prefix=f"fb-{tid}-")
    proj = os.path.join(work, "proj")
    target = os.path.join(tempfile.gettempdir(), "fb-trial-target")
    shutil.copytree(os.path.join(HERE, "fixture"), proj)
    if a.task == "F5":
        shutil.copytree(os.path.join(HERE, "overlays/f5/tests"), os.path.join(proj, "tests"), dirs_exist_ok=True)
    with open(os.path.join(proj, ".gitignore"), "w") as f:
        f.write("target/\n")
    sh(["cargo", "generate-lockfile", "--offline"], proj)
    for c in (["git", "init", "-q"], ["git", "add", "-A"],
              ["git", "-c", "user.email=h@x", "-c", "user.name=h", "commit", "-qm", "base"]):
        sh(c, proj)
    base = sh(["git", "rev-parse", "HEAD"], proj).stdout.strip()
    fixture_hash = hashlib.sha256(b"".join(read(os.path.join(dp, f)) for dp, _, fs in sorted(os.walk(os.path.join(HERE, "fixture"))) for f in sorted(fs))).hexdigest()[:16]

    port_file, plog = os.path.join(work, "port"), os.path.join(work, "proxy.jsonl")
    proxy = subprocess.Popen([sys.executable, os.path.join(HERE, "logging_proxy.py"), "--upstream", a.upstream,
                              "--port-file", port_file, "--log", plog], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(50):
        if os.path.exists(port_file) and read(port_file).strip():
            break
        time.sleep(0.1)
    port = read(port_file).decode().strip()
    cmd = [a.bin, "work", GOAL[a.task], "--kind", KIND[a.task], "--provider", "openai-compatible", "--model", MODEL,
           "--endpoint", f"http://localhost:{port}/v1/chat/completions", "--provider-profile", "local",
           "--structured-output", "object", "--max-output-tokens", str(CELLS[a.cell]),
           "--call-timeout-secs", str(CALL_TIMEOUT), "--deadline-secs", str(DEADLINE), "--json", "--print-reply"]
    t0, timed_out = time.time(), False
    try:
        r = sh(cmd, proj, timeout=DEADLINE + 120)
        stdout, stderr, code = r.stdout, r.stderr, r.returncode
    except subprocess.TimeoutExpired as e:
        timed_out, stdout, stderr, code = True, "", "", None
    elapsed = round(time.time() - t0, 2)
    proxy.terminate()
    try:
        report = json.loads(stdout)
    except Exception:
        report = {}
    replies = []
    for m in re.finditer(r"Model reply:\n(\{.*\})", stderr):
        try:
            replies.append(json.loads(m.group(1)))
        except Exception:
            replies.append({"unparsed": True})
    calls = []
    if os.path.exists(plog):
        for l in open(plog):
            x = json.loads(l)
            calls.append({k: x.get(k) for k in ("seconds", "status", "max_tokens", "response_format", "tools", "response")})
    accepted, detail = accept(a.task, proj, report, replies, target) if report else (False, {"no_report": True})
    cat = classify(a.task, accepted, report, replies, proj, target, timed_out) if report else "unknown"
    if timed_out or code is None:
        cat = "unknown"
    tr = {
        "trial": tid, "cell": a.cell, "task": a.task, "kind": KIND[a.task], "goal": GOAL[a.task],
        "fixture_hash": fixture_hash, "base_commit": base, "model": MODEL, "max_output_tokens": CELLS[a.cell],
        "call_timeout_secs": CALL_TIMEOUT, "deadline_secs": DEADLINE, "exit_code": code, "harness_timeout": timed_out,
        "elapsed_seconds": elapsed, "terminal_state": report.get("terminal_state"), "outcome_reason": report.get("outcome_reason"),
        "goal_satisfied": report.get("goal_satisfied"), "model_failures": report.get("model_failures"),
        "invalid_decisions": report.get("invalid_decisions"), "invalid_inputs": report.get("invalid_inputs"),
        "capability_requests": report.get("capability_requests"), "executions_by_capability": report.get("executions_by_capability"),
        "reads": report.get("reads"), "lists": report.get("lists"), "writes": report.get("writes"),
        "paths_written": report.get("paths_written"), "pax_executions": report.get("pax_executions"), "tests": report.get("tests"),
        "model_calls": (report.get("measurement") or {}).get("model_calls"),
        "model_replies": replies, "proxy_calls": calls,
        "stuck_guard_event": str(report.get("outcome_reason") or "").startswith("stuck: "),
        "output_limit_event": any(f.get("kind") == "length_limited" for f in (report.get("model_failures") or [])),
        "timeout_flag": bool(timed_out or "timed out" in str(report.get("outcome_reason") or "").lower() or "deadline" in str(report.get("outcome_reason") or "").lower()),
        "acceptance": {"passed": accepted, **detail}, "final_category": cat,
    }
    with open(os.path.join(a.out, f"{tid}.json"), "w") as f:
        json.dump(tr, f, indent=1)
    print(tid, cat, f"{elapsed}s")
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
