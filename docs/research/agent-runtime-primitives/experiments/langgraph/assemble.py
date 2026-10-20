"""Merge harness outputs from <workdir> into results.json and copy small raw logs into raw/.

usage: python -I assemble.py <workdir> <commit-sha>
"""
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
work, sha = sys.argv[1], sys.argv[2]
L = lambda n: json.load(open(os.path.join(work, n)))  # noqa: E731
e1, e5 = L("e1.json"), L("e5.json")
sync, asy, ext, conc, e3 = L("e2_sync.json"), L("e2_async.json"), L("e2_exit.json"), L("e2_concurrent.json"), L("e3.json")

KEYS = ["resumed", "charges_in_ledger", "final_state_consistent", "progress_lost", "manual_intervention", "outcome",
        "killed_at", "repetitions", "outcomes_over_reps", "charges_over_reps", "charge_task_ids"]
NOTES = {
    "W1": "Kill at charge entry before its ledger write; the agent step that chose charge was checkpointed. Resume re-runs the tools node once.",
    "W2": "Kill after the charge ledger fsync, before return. The tools node never finished, so nothing about it was recorded; resume re-runs the whole node -> second charge. Both ledger lines carry the same task_id and tool_call_id: a stable key is derivable but LangGraph does not forward or check one.",
    "W3": "Kill right after the tools task's pending write (charge ToolMessage) committed. Pending write (or the already-saved superstep checkpoint) is reapplied; the tools node is not re-run.",
    "W3b": "Kill at the start of model call 3, after the superstep checkpoint. Resume re-runs only the agent node.",
    "W4": "Kill inside SqliteSaver.put for the checkpoint that carries the charge result (statements executed, transaction uncommitted). NOT deterministic in outcome even though the kill point is: the tools task's put_writes is submitted to a thread pool and is not ordered before the checkpoint put; when put won the lock first, the charge result was in neither table -> tools node re-run -> duplicate (2/5).",
    "W4b": "Kill inside put_writes of the charge result, before commit. Result lost; resume re-runs the tools node -> duplicate.",
}
windows = {}
for w, r in sync.items():
    windows[w] = {k: r.get(k) for k in KEYS}
    windows[w]["notes"] = NOTES[w]
w5 = e3["W5_kill_during_handoff"]
windows["W5"] = {"resumed": True, "charges_in_ledger": w5["charges_in_ledger"], "final_state_consistent": True,
                 "progress_lost": False, "manual_intervention": "none: invoke(None) then Command(resume=approve)",
                 "outcome": "correct" if w5["finished"] and w5["charges_in_ledger"] == 1 else "check",
                 "killed_at": w5["killed_at"],
                 "notes": "Kill right after triage's Command(goto='specialist') write committed. Fresh process: next=['specialist']; specialist re-ran from its start with the same task_id (its in-flight model call 1 was repeated: it had not been recorded)."}
w6 = e3["W6_approval_across_restart"]
windows["W6"] = {"resumed": True, "charges_in_ledger": w6["charges_in_ledger"], "final_state_consistent": True,
                 "progress_lost": False, "manual_intervention": "a person supplies Command(resume={'approve': True}) (by design)",
                 "outcome": "correct" if w6["finished"] and w6["charges_in_ledger"] == 1 else "check",
                 "killed_at": w6["killed_at"],
                 "notes": "SIGKILL while suspended in interrupt(). Fresh process sees next=['approval'] and the interrupt id/value; approval node re-executes from its start and interrupt() returns the resume value. A second identical Command(resume) after completion was a no-op (charges stay 1)."}
summ = lambda d: {w: {"outcomes_over_reps": r["outcomes_over_reps"], "charges_over_reps": r["charges_over_reps"],  # noqa: E731
                      "killed_at": r["killed_at"], "resume_error": r.get("resume_error")} for w, r in d.items()}
results = {
    "candidate": "langgraph",
    "commit": sha,
    "package": "langgraph==1.2.16 (+ langgraph-checkpoint==4.3.0, langgraph-checkpoint-sqlite==3.1.2, langgraph-prebuilt==1.1.1, langchain-core==1.6.12)",
    "environment": {**e1["environment"]},
    "fake_model": "custom langchain_core BaseChatModel subclass (ScriptedChatModel); reply index = AI replies in restored state + calls in this process",
    "e1": e1["e1"],
    "e2": {
        "store": "sqlite (SqliteSaver, WAL, file on local disk), durability='sync' (most durable built-in mode)",
        "resume_api": "graph.invoke(None, {'configurable': {'thread_id': ...}}) in a fresh process",
        "windows": windows,
        "supplementary": {
            "durability_async": summ(asy),
            "durability_exit": {**summ(ext), "note": "exit mode writes nothing until the run exits; after SIGKILL there is no checkpoint, invoke(None) raises EmptyInputError (all progress lost, effectively lost_progress). W3/W4b hooks never fire in exit mode (no put_writes during the run), so those runs completed un-killed and are not_applicable."},
            "concurrent_resume_no_fence": {k: conc[k] for k in ("charges_in_ledger", "charge_pids", "charge_task_ids", "final_state_charge_toolmsgs", "outcome")}
            | {"note": "After a W1 crash, two fresh processes resumed the same thread; no owner/lease/fence exists, both executed charge (same task_id). Final state shows 1 charge ToolMessage vs 2 in the ledger."},
        },
    },
    "e3": {
        "supported": True,
        "flow": "triage --Command(goto='specialist')--> specialist --(charge proposed)--> approval node [interrupt()] --> ToolNode --> specialist",
        "context_passed": "full shared state: the `messages` channel (add_messages reducer) is the same list for every node; specialist saw 3 messages (human, triage AI tool-call, transfer ToolMessage). Filtering/summaries are app code.",
        "state_owner": "the graph run (Pregel loop) owns state; nodes only return writes; the checkpointer stores it keyed by thread_id/checkpoint_ns/checkpoint_id. `active_agent` is an app-level field.",
        "execution_ids": "thread_id (caller-chosen), checkpoint_id (uuid6 per superstep), task_id (deterministic hash of checkpoint id+ns+step+node+trigger; identical when a task is re-executed after a crash), interrupt id (xxh3 of checkpoint_ns), tool_call_id (from the model).",
        "specialist_fails_no_retry": {"charges": e3["specialist_fails_no_retry"]["charges_in_ledger"], "finished": e3["specialist_fails_no_retry"]["finished"],
                                      "notes": "invoke raised RuntimeError; error stored as pending ERROR write (get_state().tasks[].error); next=['specialist']; a fresh process invoke(None) re-ran specialist (same task_id) and continued."},
        "specialist_fails_with_retry": {"charges": e3["specialist_fails_with_retry"]["charges_in_ledger"], "finished": e3["specialist_fails_with_retry"]["finished"],
                                        "notes": "RetryPolicy(max_attempts=2): attempt 2 in-process, same task_id, execution_info.node_attempt=2. Retry state is not persisted across processes."},
        "approval_survives_restart": True,
        "double_approval_after_completion": "no-op, charges=1",
    },
    "e5": e5["e5"],
    "blocked": ["E4: no local inference endpoint (protocol)"],
}
json.dump(results, open(os.path.join(HERE, "results.json"), "w"), indent=1, default=str)
raw = os.path.join(HERE, "raw")
os.makedirs(raw, exist_ok=True)
for n in ("e1.json", "e2_sync.json", "e2_async.json", "e2_exit.json", "e2_concurrent.json", "e3.json", "e5.json"):
    shutil.copy(os.path.join(work, n), os.path.join(raw, n))
print("ok")
