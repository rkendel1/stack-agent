#!/usr/bin/env python3
"""Builds results.json (protocol shape) from logs/*_raw.json. Usage: assemble.py EXPERIMENT_DIR"""
import json
import os
import platform
import subprocess
import sys

d = sys.argv[1]
L = lambda n: json.load(open(os.path.join(d, "logs", f"{n}_raw.json")))
e1, e2, e5 = L("e1"), L("e2"), L("e5")
rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()

windows = {}
for name, w in e2.items():
    if "crash_run" not in w or name in ("W2-default-lease", "W2-refusal-default-lease"):
        windows[name] = w
        continue
    out = {
        "kill_point": w["kill_point"],
        "kill_reached": w["kill_reached"],
        "persisted_at_kill": {
            "db_transactions": w["db_transactions_at_kill"],
            "db_torn_tail_bytes": w.get("db_torn_tail_bytes"),
            "intents": [i[1] + ":" + i[2] for i in (w["history_at_kill"] or {}).get("intents", [])],
            "task_status": (w["history_at_kill"] or {}).get("status"),
            "ledger_lines": w["ledger_at_kill"],
        },
        "resumed": w.get("resumed"),
        "first_resume_exit": w["resume"]["exit"],
        "first_resume_refusal": w["resume"]["stderr_tail"] if w["resume"]["exit"] else None,
        "charges_in_ledger": w["charges_in_ledger"],
        "ledger_invocations": w.get("ledger_invocations"),
        "final_task_status": w.get("final_task_status"),
        "final_state_consistent": w.get("final_state_consistent"),
        "progress_lost": w.get("progress_lost"),
        "manual_intervention": None,
        "outcome": w["outcome"],
    }
    if "manual_intervention" in w:
        mi = w["manual_intervention"]
        if isinstance(mi, dict):
            out["manual_intervention"] = (
                f"wait out the killed owner's lease; resume refused (exit 3, OutcomeUncertain) but it acquired the task "
                f"(fence {w.get('fence_after_refused_resume')}) under a new lease; operator wrote chip.Observation "
                f"executed={mi['conclusion_from_ledger'] == 'executed'} for {mi['open_intents']} from the ledger (sa-state reconcile); "
                f"wait out the refused resume's lease; resume again")
            out["outcome_after_manual_resolution"] = w["outcome_after_manual_resolution"]
            out["after_manual_resolution"] = {
                "exit": w["resume_after_manual"]["exit"],
                "executions_by_capability": w["resume_after_manual"]["executions_by_capability"],
                "attempt": (w["resume_after_manual"]["agent_state"] or {}).get("attempt"),
            }
        else:
            out["manual_intervention"] = mi
    else:
        out["manual_intervention"] = "wait out the killed owner's lease (--lease-secs; default 900 s), then rerun the same command"
    out["resume_executions_by_capability"] = (w.get("resume") or {}).get("executions_by_capability")
    windows[name] = out
windows["W2-default-lease"] = {k: e2["W2-default-lease"][k] for k in ("kill_point", "charges_in_ledger", "outcome", "manual_intervention")}
windows["W2-default-lease"]["resume_exit"] = e2["W2-default-lease"]["resume"]["exit"]
windows["W2-default-lease"]["resume_refusal"] = e2["W2-default-lease"]["resume"]["stderr_tail"]

m = e1["main"]
res = {
    "candidate": "stack-agent",
    "commit": "366f6c1",
    "commit_note": "origin/main @ 366f6c1 (Merge pull request #10), checkout /home/user/stack-agent-research",
    "package": "stack-agent release binary (cargo build --locked --release --bin stack-agent); feltdb 0.2.0 @ rkendel1/flow_db b24481360f7c10564ae09fcc5bbf7802725eccd9 (from the local cargo git cache, no network)",
    "environment": {"cpus": os.cpu_count(), "rust": rustc, "python": platform.python_version(),
                    "os": platform.platform(), "machine": platform.machine()},
    "fake_model": "fake_model.py: OpenAI-compatible SSE chat-completions server (stdlib); reply chosen from the request's content (no read observation -> project.read; read observed -> pax.test), so a restarted process gets the right next decision",
    "fixture_mapping": {"lookup": "project.read src/lib.rs (Chip's own executor; not ledgered)",
                        "charge": "pax.test (fake `pax` on PATH appends+fsyncs ledger.jsonl, then prints pax.execution-result.v1 passed)",
                        "final_answer": "none: --kind verify is completed by Chip's local policy once pax.test passed (no third model call)",
                        "goal": "Establish whether the project passes its tests. (--kind verify, tiny Cargo fixture, cargo never runs)"},
    "e1": {
        "success": m["success"], "store": "FeltDB file (--state-db)",
        "model_calls": m["model_calls"], "tool_calls": m["tool_calls"],
        "executions_by_capability": m["executions_by_capability"], "charges_in_ledger": m["charges_in_ledger"],
        "task_status": m["task_status"], "decisions": m["decisions"], "retries": 0, "tokens": m["tokens"],
        "tokens_note": "the fake model reports 1+1 usage per call", "wall_ms": m["wall_ms"],
        "idempotency_key_forwarded": False,
        "idempotency_key_note": "chip-agent-state computes intent key a<attempt>.<execution id> but the inner executor (pax) receives no key [source chip-agent-state/src/lib.rs:1143-1151]; the fake pax saw only `--dir <root> --json test`",
        "invalid_args_handled": {k.replace("invalid_", ""): v for k, v in e1.items() if k.startswith("invalid_")},
    },
    "e2": {"store": "FeltDB file-backed log (--state-db FILE --task-id ID --lease-secs N), reopened by every new process",
           "kill_mechanisms": {
               "W1a": "fake pax SIGKILLs its parent on the `pax --version` identity probe chip-pax runs while validating the chosen pax.test request",
               "W1a-model": "fake model SIGKILLs the agent pid (pidfile written by the launching shell before exec) when asked the decision that leads to pax.test",
               "W1b/W2": "fake pax SIGKILLs its parent before / after the fsynced ledger line",
               "W3/W4*": "gdb_kill.py: ptrace syscall catchpoints on the state-db file descriptor; SIGKILL at an exact commit boundary or inside a commit's write() sequence. No code change, no timing"},
           "lease_note": "W1/W2 crash runs use --lease-secs 1, gdb runs 30 (ptrace slows the agent); the harness waits lease+0.5 s before each resume. Waiting is for lease expiry, never a kill trigger",
           "windows": windows},
    "e3": {"supported": False, "notes": "no handoff, delegation or approval primitive in stack-agent (baseline §4); not run"},
    "e5": e5,
    "blocked": ["E4: no local inference endpoint (protocol)"],
}
res["e5"]["notes"] = ("wall time of one `stack-agent work` process (fresh project and data dir per run, fake model already "
                      "listening): includes process start, provider setup, 2 HTTP model calls to the Python fake model, "
                      "2 executions (project.read, fake pax = a Python process start), learning-store writes; durable adds "
                      "FeltDB open + 12 fdatasync'd commits. in_memory = no --state-db (legacy).")
json.dump(res, open(os.path.join(d, "results.json"), "w"), indent=1)
print("ok")
