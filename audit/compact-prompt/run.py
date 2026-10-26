#!/usr/bin/env python3
"""A/B evaluation of the decision prompt (`full` vs `compact`) in the real Stack-Agent work loop.

    python3 audit/compact-prompt/run.py --bin target/release/stack-agent --repo <pristine copy of the
        repository> --upstream http://127.0.0.1:1338 --model woodcock-qwen3-4b --out <dir>
        [--trials 5,3,3] [--only T1] [--summarize-only]

What it does, per trial: copies the pristine repository (so no trial sees another's state or writes to
the real checkout), runs `stack-agent work --kind inspect --json --print-reply "<goal>"` with the
SAME environment, limits and model settings in both modes (only CHIP_DECISION_PROMPT differs), through
`proxy.py` so every request and reply is logged independently of Stack-Agent's own accounting, then
reads Stack-Agent's durable event trail (`agent-state show`) for what the runtime decided.

Order: for each task and trial, `full` then `compact`, interleaved, so drift in the server affects
both alike. Nothing is retried, tuned or re-run after seeing a result.

SCORING IS DEFINED HERE, BEFORE ANY COMPACT RESULT WAS SEEN. Five different measures, never merged:

  syntactic validity   the reply is, as a whole, one JSON object (python json.loads of the trimmed text)
  contract validity    Stack-Agent's own `DecisionMade` (accepted) versus `DecisionRejected` event for
                       that model call. The runtime's boundary decides this, not this script.
  decision correctness for an ACCEPTED request, an oracle below says whether the capability and its
                       inputs are appropriate for the task and the evidence so far. A valid decision
                       that is not appropriate (an invented capability, a read of an unrelated file)
                       counts as a failure of correctness, not as a partial success.
  execution correctness the runtime's execution events: requests that ran, and reads that succeeded
  task success         completed + grounded (Stack-Agent's own check that the answer cites an observed
                       file) + the answer cites at least TWO distinct files under the task's target
                       that were actually read. This is a heuristic: it does not verify that the claims
                       in the answer are true.

Decision oracle, per task (`target` is the directory the goal is about):
  project.list    appropriate if the path is "." / "" or on the way to the target (a prefix of it) or
                  inside it
  project.search  appropriate if the path is absent or on the way to / inside the target
  project.read    appropriate only if the path is inside the target
  anything else   not appropriate for these read-only tasks (the capability may exist; it is not
                  needed here)
  complete        appropriate only after at least one successful read inside the target
"""
import argparse, json, os, shutil, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent

TASKS = {
    "T1": {
        "target": "crates/fx-native-provider",
        "goal": "Inspect the FX native inference provider implementation (crates/fx-native-provider) "
                "and explain its request construction, response validation, and error handling. "
                "Cite file paths for each claim.",
        "topics": ["request", "valid", "error"],
    },
    "T2": {
        "target": "crates/chip-pax",
        "goal": "Inspect the PAX executor adapter (crates/chip-pax) and explain how it finds the pax "
                "executable, checks that it really is PAX, and treats a result it cannot trust. "
                "Cite file paths for each claim.",
        "topics": ["locate", "version", "fail"],
    },
    "T3": {
        "target": "crates/chip-project",
        "goal": "Inspect the project file capabilities (crates/chip-project) and explain which paths a "
                "model may read and which are withheld, and how a path is confined to the project root. "
                "Cite file paths for each claim.",
        "topics": ["withheld", "path", "root"],
    },
}


def on_the_way(path, target):
    p = path.strip().strip("/")
    return p in ("", ".") or target == p or target.startswith(p + "/")


def inside(path, target):
    p = path.strip().strip("/")
    return p == target or p.startswith(target + "/")


def appropriate(cap, inputs, target, reads_in_target):
    inputs = inputs or {}
    path = str(inputs.get("path", "")) if "path" in inputs else None
    if cap == "project.list":
        return path is None or on_the_way(path, target) or inside(path, target)
    if cap == "project.search":
        return path is None or on_the_way(path, target) or inside(path, target)
    if cap == "project.read":
        return path is not None and inside(path, target)
    return False


def strict_json_object(text):
    try:
        v = json.loads(text.strip())
    except ValueError:
        return None
    return v if isinstance(v, dict) else None


def start_proxy(upstream, log):
    p = subprocess.Popen([sys.executable, str(HERE / "proxy.py"), "--upstream", upstream, "--log", str(log)],
                         stdout=subprocess.PIPE, text=True)
    line = p.stdout.readline().strip()
    assert line.startswith("listening "), line
    return p, int(line.split()[1])


def run_trial(args, port, task_id, task, mode, trial, workroot):
    label = f"{task_id}-{mode}-{trial}"
    work = workroot / label
    shutil.copytree(args.repo, work / "repo", symlinks=True)
    env = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": str(work / "home"),
        "STACK_AGENT_DATA_DIR": str(work / "data"),
        "CHIP_MODEL": args.model,
        "CHIP_ENDPOINT": f"http://127.0.0.1:{port}/{label}/v1/chat/completions",
        "CHIP_CALL_TIMEOUT_SECS": str(args.call_timeout),
    }
    if mode == "compact":
        env["CHIP_DECISION_PROMPT"] = "compact"
    cmd = [args.bin, "work", "--kind", "inspect", "--json", "--print-reply", task["goal"]]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, cwd=work / "repo", env=env, capture_output=True, text=True, timeout=args.trial_timeout)
        rc, out, err, timed_out = p.returncode, p.stdout, p.stderr, False
    except subprocess.TimeoutExpired as e:
        rc, out, err, timed_out = None, (e.stdout or ""), (e.stderr or ""), True
    wall = time.time() - t0
    try:
        report = json.loads(out)
    except ValueError:
        report = None
    events = []
    if report and report.get("agent_state", {}).get("task_id"):
        s = subprocess.run([args.bin, "agent-state", "show", "--task-id", report["agent_state"]["task_id"], "--json"],
                           cwd=work / "repo", env=env, capture_output=True, text=True)
        try:
            events = [e["event"] for e in sorted(json.loads(s.stdout)["events"], key=lambda e: e["index"])]
        except (ValueError, KeyError):
            events = []
    return dict(label=label, rc=rc, timed_out=timed_out, wall_s=round(wall, 2), report=report,
                events=events, stderr_tail=err[-400:])


def score(trial_result, calls, task, mode, task_id, n):
    target = task["target"]
    ev, rep = trial_result["events"], trial_result["report"]
    calls = [c for c in calls if c["label"] == trial_result["label"]]
    calls.sort(key=lambda c: c["at"])
    # Pair each model call with the runtime's decision about its reply, in order.
    verdicts, reasons = [], []
    for e in ev:
        if e["kind"] == "DecisionMade":
            verdicts.append(True); reasons.append(None)
        elif e["kind"] == "DecisionRejected":
            verdicts.append(False); reasons.append(e.get("reason", ""))
    rows, reads_in_target, read_ok_paths = [], 0, []
    for i, c in enumerate(calls):
        obj = strict_json_object(c["reply"])
        accepted = verdicts[i] if i < len(verdicts) else None
        row = dict(i=i, syntactic=obj is not None, accepted=accepted, reason=reasons[i] if i < len(reasons) else None,
                   reply=c["reply"], prompt_chars=c["prompt_chars"], system_chars=c["system_chars"],
                   usage=c["usage"], latency_s=c["latency_s"], finish=c["finish_reason"])
        if obj and obj.get("decision") == "request_capability":
            cap, inp = obj.get("capability"), obj.get("inputs")
            row.update(kind="request", capability=cap, inputs=inp)
            if accepted:
                row["appropriate"] = appropriate(cap, inp, target, reads_in_target)
        elif obj:
            row.update(kind=obj.get("decision"))
            if obj.get("decision") == "complete" and accepted:
                row["appropriate"] = reads_in_target > 0
        # a read inside the target that the runtime then executed successfully
        if accepted and row.get("kind") == "request" and row.get("capability") == "project.read" and row.get("appropriate"):
            reads_in_target += 1
            read_ok_paths.append(str(row["inputs"].get("path")))
        rows.append(row)
    answer = (rep or {}).get("answer") or ""
    cited = sorted({p for p in set(read_ok_paths) if p in answer})
    completed = bool(rep) and rep.get("terminal_state") == "completed"
    grounded = bool(rep) and bool(rep.get("grounded"))
    task_success = completed and grounded and len(cited) >= 2
    exec_started = sum(1 for e in ev if e["kind"] == "ExecutionStarted")
    exec_ok = sum(1 for e in ev if e["kind"] == "ExecutionCompleted")
    return dict(
        task=task_id, mode=mode, trial=n, label=trial_result["label"], rc=trial_result["rc"],
        timed_out=trial_result["timed_out"], wall_s=trial_result["wall_s"],
        terminal_state=(rep or {}).get("terminal_state"), outcome_reason=(rep or {}).get("outcome_reason"),
        model_calls=len(calls),
        first_prompt_chars=calls[0]["prompt_chars"] if calls else None,
        first_system_chars=calls[0]["system_chars"] if calls else None,
        input_tokens=sum((c["usage"] or {}).get("prompt_tokens", 0) for c in calls) if calls else None,
        output_tokens=sum((c["usage"] or {}).get("completion_tokens", 0) for c in calls) if calls else None,
        model_latency_s=round(sum(c["latency_s"] for c in calls), 2),
        syntactic_valid=sum(r["syntactic"] for r in rows),
        contract_valid=sum(1 for r in rows if r["accepted"]),
        contract_rejected=sum(1 for r in rows if r["accepted"] is False),
        requests_accepted=sum(1 for r in rows if r["accepted"] and r.get("kind") == "request"),
        capability_correct=sum(1 for r in rows if r["accepted"] and r.get("kind") == "request" and r.get("capability") in ("project.list", "project.search", "project.read")),
        decisions_appropriate=sum(1 for r in rows if r.get("appropriate")),
        decisions_inappropriate=sum(1 for r in rows if r["accepted"] and r.get("appropriate") is False),
        executions_started=exec_started, executions_completed=exec_ok,
        reads_ok=(rep or {}).get("reads"), failed_observations=(rep or {}).get("failed_observations"),
        completed=completed, grounded=grounded, cited_target_files=cited, task_success=task_success,
        malformed=(rep or {}).get("malformed_replies"),
        replies=rows,
    )


def summarize(results):
    out = ["# Results (generated; see results.jsonl for every trial and reply)\n"]
    for task_id in sorted({r["task"] for r in results}):
        out.append(f"\n## {task_id}\n")
        out.append("| mode | trials | calls | first prompt chars | json valid | contract valid | rejected | appropriate / accepted requests | executions | reads ok | completed | task success | in tokens/trial | latency s/trial |")
        out.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for mode in ("full", "compact"):
            rs = [r for r in results if r["task"] == task_id and r["mode"] == mode]
            if not rs:
                continue
            calls = sum(r["model_calls"] for r in rs)
            acc_req = sum(r["requests_accepted"] for r in rs)
            apprR = sum(1 for r in rs for x in r["replies"] if x.get("kind") == "request" and x.get("appropriate"))
            tok = [r["input_tokens"] for r in rs if r["input_tokens"] is not None]
            out.append("| {m} | {n} | {c} | {fp} | {sv}/{c} | {cv}/{c} | {rj} | {ap}/{ar} | {ex} | {rd} | {co}/{n} | {ts}/{n} | {tk} | {lat} |".format(
                m=mode, n=len(rs), c=calls, fp=sorted({r["first_prompt_chars"] for r in rs if r["first_prompt_chars"]}),
                sv=sum(r["syntactic_valid"] for r in rs), cv=sum(r["contract_valid"] for r in rs),
                rj=sum(r["contract_rejected"] for r in rs), ap=apprR, ar=acc_req,
                ex=sum(r["executions_completed"] for r in rs), rd=sum((r["reads_ok"] or 0) for r in rs),
                co=sum(r["completed"] for r in rs), ts=sum(r["task_success"] for r in rs),
                tk=round(sum(tok) / len(tok)) if tok else "n/a",
                lat=round(sum(r["model_latency_s"] for r in rs) / len(rs), 1)))
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin"); ap.add_argument("--repo"); ap.add_argument("--upstream"); ap.add_argument("--model")
    ap.add_argument("--out", required=True)
    ap.add_argument("--trials", default="5,3,3", help="trials per mode for T1,T2,T3")
    ap.add_argument("--only"); ap.add_argument("--call-timeout", type=int, default=180)
    ap.add_argument("--trial-timeout", type=int, default=900)
    ap.add_argument("--summarize-only", action="store_true")
    args = ap.parse_args()
    # Trials run in a copy of the repository, so every path given must be absolute.
    for name in ("bin", "repo", "out"):
        if getattr(args, name):
            setattr(args, name, str(Path(getattr(args, name)).resolve()))
    out = Path(args.out); out.mkdir(parents=True, exist_ok=True)
    if args.summarize_only:
        results = [json.loads(l) for l in open(out / "results.jsonl")]
        (out / "summary.md").write_text(summarize(results)); print(summarize(results)); return
    log = out / "calls.jsonl"
    open(log, "w").close()
    proxy, port = start_proxy(args.upstream, log)
    rev = subprocess.run(["git", "-C", args.repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    (out / "meta.json").write_text(json.dumps(dict(
        binary=args.bin, binary_version=subprocess.run([args.bin, "--version"], capture_output=True, text=True).stdout.strip(),
        task_repo_revision=rev, model=args.model, upstream=args.upstream, call_timeout_secs=args.call_timeout,
        trials=args.trials, started=time.strftime("%Y-%m-%dT%H:%M:%S%z"), tasks=TASKS), indent=2))
    counts = dict(zip(["T1", "T2", "T3"], [int(x) for x in args.trials.split(",")]))
    workroot = out / "work"; workroot.mkdir(exist_ok=True)
    results = []
    try:
        for task_id, task in TASKS.items():
            if args.only and task_id != args.only:
                continue
            for n in range(1, counts[task_id] + 1):
                for mode in ("full", "compact"):
                    r = run_trial(args, port, task_id, task, mode, n, workroot)
                    calls = [json.loads(l) for l in open(log)]
                    s = score(r, calls, task, mode, task_id, n)
                    results.append(s)
                    with open(out / "results.jsonl", "a") as f:
                        f.write(json.dumps(s) + "\n")
                    print(f"{task_id} {mode:7} trial {n}: calls={s['model_calls']} json={s['syntactic_valid']} contract={s['contract_valid']} "
                          f"rejected={s['contract_rejected']} state={s['terminal_state']} success={s['task_success']} ({s['wall_s']}s)", flush=True)
                    shutil.rmtree(workroot / r["label"], ignore_errors=True)
    finally:
        proxy.terminate()
    (out / "summary.md").write_text(summarize(results))
    print(summarize(results))


if __name__ == "__main__":
    main()
