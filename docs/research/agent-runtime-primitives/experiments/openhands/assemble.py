"""Builds results.json from logs/*.json and the run directories (read-only).

    python3 -I assemble.py <runs-dir>

Per case it re-derives, from the persisted event files, the active branch the conversation
presents after the resume (base_state.json ``leaf_event_id`` walked through ``parent_id``),
so ``final_state_consistent`` and ``progress_lost`` are judged on what the conversation shows,
not on the raw file count.
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOGS = HERE / "logs"
CONV_HEX = "000000000000400080000000000000e2"

GROUP_NOTES = {
    "W1-sdk": "Kill at charge entry (ActionEvent fsynced, no ledger line). HEAD (leaf_event_id in base_state.json) is saved only when the step's `with state:` block exits, so the charge ActionEvent is off the active branch on reload. The bare SDK sees no pending action; the model is asked again. count rule: it answers final with no charge (a false completion); visible rule: it re-decides charge (1 charge). The original ActionEvent stays unmatched in the full log forever.",
    "W2-sdk": "Kill inside charge after the ledger fsync. Same HEAD lag: the charge ActionEvent is off-branch, so implicit confirmation does NOT fire. count rule: final answer, 1 charge, but the branch shows no charge (state disagrees with ledger). visible rule: the model is shown only the lookup and charges again: duplicate_effect.",
    "W3-sdk": "Kill at the start of model call 3; the step that ran charge had ended, so HEAD covers the charge observation. Resume makes one model call.",
    "W4a-sdk": "Kill in the EventLog write guard before the charge ObservationEvent file is written (charge already ran). Same as W2.",
    "W4b-sdk": "Kill after the charge ObservationEvent file is written, before the in-memory index, length marker and HEAD advance. The observation is on disk but off-branch: visible rule charges again.",
    "W6-sdk": "AlwaysConfirm; SIGKILL while WAITING_FOR_CONFIRMATION on charge. The step ended, so HEAD covers the pending ActionEvent and the status persisted. Approval after restart = run(), which executes the pending action (implicit confirmation): 1 charge. Caveat: on cold load the view's ToolCallMatchingProperty drops the unmatched ActionEvent from the LLM view, so the next model call is shown a charge tool result whose tool call is missing (visible_assistant=1 with 2 tool results).",
    "W6-sdk-count-reject": "As W6, then reject_pending_actions() in the new process: UserRejectObservation, 0 charges, model gives its final reply.",
    "W1-server": "EventService.start(): dead-owner lease takeover (generation 1 -> 2), RUNNING -> ERROR, full-log scan finds the unmatched charge ActionEvent and appends AgentErrorEvent 'A restart occurred while this tool was in progress ... did not complete', parented to the action. The charge never ran; the scripted model (both rules) takes the error as the charge result and gives its final reply: 0 charges, a false completion. Indistinguishable from W2 for the runtime.",
    "W2-server": "Same recovery facts as W1: synthetic error, no re-execution, 1 charge. The record says the tool 'did not complete' although the ledger shows it did: the ambiguity is converted into a false negative, not surfaced as unknown.",
    "W2-server-count-retry-on-error": "Sensitivity: a model that re-issues charge after the restart error. 2 charges. The server rule surfaces, it does not protect.",
    "W1-server-count-retry-on-error": "Sensitivity: the same model in W1 gives 1 charge (correct). W1 and W2 leave identical recovery facts, so no model policy is right in both.",
    "W3-server": "RUNNING -> ERROR on load; nothing unmatched; one model call to the final answer.",
    "W4a-server": "Like W2-server.",
    "W4b-server": "Charge ObservationEvent file on disk but off the active branch. The full-log scan sees the action as matched, so no synthetic error and no re-attachment: the active branch still ends at the lookup. visible rule charges again (duplicate); count rule answers final with the charge invisible on the branch.",
    "W6-server": "Status WAITING_FOR_CONFIRMATION persisted, not RUNNING, so the recovery rule does not apply. Accept (= run(), as respond_to_confirmation(accept=True) does) executes the pending charge: 1 charge.",
    "W2c-sdk": "Confirmation mode, the person approved charge (run()), the process died inside charge after the ledger fsync. run() flips WAITING_FOR_CONFIRMATION -> RUNNING inside the same `with state:` block as the step, so that save is deferred and the persisted status is still WAITING_FOR_CONFIRMATION. After restart the action looks un-approved; approving again (run()) re-executes it via implicit confirmation: duplicate_effect with no model involvement.",
    "W2c-server": "Same as W2c-sdk: the persisted status is WAITING_FOR_CONFIRMATION, so the stale-RUNNING rule never fires; accept re-executes the charge: duplicate_effect.",
}


def group_of(case: str) -> str:
    for g in sorted(GROUP_NOTES, key=len, reverse=True):
        if case.startswith(g):
            return g
    return case


def branch(conv: Path) -> tuple[list[dict], list[dict]]:
    events = []
    for p in sorted((conv / "events").glob("*.json")):
        events.append(json.loads(p.read_text()))
    by_id = {e["id"]: e for e in events}
    leaf = json.loads((conv / "base_state.json").read_text()).get("leaf_event_id")
    path, cur = [], leaf
    while cur and cur in by_id:
        path.append(by_id[cur])
        cur = by_id[cur].get("parent_id")
    return events, list(reversed(path))


def charge_obs_ok(e: dict) -> bool:
    if e.get("kind") != "ObservationEvent" or e.get("tool_name") != "charge":
        return False
    text = "".join(c.get("text", "") for c in e.get("observation", {}).get("content", []))
    return text.startswith("charged")


def main() -> None:
    runs = Path(sys.argv[1])
    windows = {}
    for log in sorted(LOGS.glob("W*.json")):
        d = json.loads(log.read_text())
        name = d["case"]
        r = d["resume"]
        conv = runs / name / "conv" / CONV_HEX
        events, br = branch(conv)
        n_before = r["persisted_before_resume"]["event_files"]
        pre = [e for e in events[:n_before] if e.get("kind") in ("ActionEvent", "ObservationEvent")]
        br_ids = {e["id"] for e in br}
        progress_lost = any(e["id"] not in br_ids for e in pre)
        charges = r["ledger"]["charge"]
        obs_on_branch = sum(1 for e in br if charge_obs_ok(e))
        after = r.get("after") or {}
        finished = after.get("status") == "finished"
        if r.get("resume_error"):
            outcome = "crashed_on_resume"
        elif "--reject" in d["resume_args"]:
            # The person rejected the charge: the right result is no charge and a finished run.
            outcome = "correct" if charges == 0 and finished else "duplicate_effect" if charges else "stuck"
        elif charges >= 2:
            outcome = "duplicate_effect"
        elif charges == 1 and finished:
            outcome = "correct"
        elif charges == 0 and finished:
            outcome = "lost_progress"
        else:
            outcome = "stuck"
        loaded = r.get("loaded") or r.get("loaded_after_recovery_rule") or {}
        windows[name] = {
            "window": d["window"], "path": d["path"], "index_rule": r.get("index_rule"),
            "resume_args": d["resume_args"],
            "killed_by_sigkill": d["killed_by_sigkill"],
            "resumed": d["resume_returncode"] == 0 and not r.get("resume_error"),
            "charges_in_ledger": charges,
            "final_state_consistent": obs_on_branch == charges,
            "charge_observations_on_active_branch": obs_on_branch,
            "progress_lost": progress_lost,
            "manual_intervention": ("person rejected the pending action" if "--reject" in d["resume_args"]
                                    else "none (approval = run(), as documented)" if d["window"] in ("W6", "W2c")
                                    else "none"),
            "outcome": outcome,
            "persisted_status_at_kill": r["persisted_before_resume"]["base_state_status"],
            "status_after_load": loaded.get("status"),
            "pending_on_active_branch_at_load": loaded.get("unmatched_active_branch"),
            "pending_in_full_log_at_load": loaded.get("unmatched_full_log"),
            "synthetic_errors": loaded.get("charge_errors"),
            "lease_generation_after_start": (r.get("lease_after_start") or {}).get("generation"),
            "model_calls_in_resume": r.get("model_calls_in_resume"),
            "notes": GROUP_NOTES.get(group_of(name), ""),
        }
    e1 = json.loads((LOGS / "e1.json").read_text())
    e1i = json.loads((LOGS / "e1_invalid.json").read_text())
    e5 = json.loads((LOGS / "e5.json").read_text())
    lease = json.loads((LOGS / "lease.json").read_text())
    res = {
        "candidate": "openhands (software-agent-sdk)",
        "commit": "33d8aa29c3078d074799e2fbf5e847aeb2d71360",
        "package": "openhands-sdk==1.56.0, openhands-agent-server==1.56.0, openhands-tools==1.56.0 (PyPI; byte-identical to the pinned source for local_conversation.py, state.py, event_store.py, agent.py, response_dispatch.py, utils/files.py, event_service.py, conversation_lease.py), litellm==1.105.1",
        "environment": {"cpus": 4, "python": "3.13.16", "os": "Linux 6.18.44"},
        "fixture": {
            "model": "ScriptedLLM, a subclass of openhands.sdk.testing.TestLLM; LLM.completion/acompletion of every LLM instance are routed to it (the agent server re-validates the agent on start and loads it from base_state.json on resume, which yields a plain LLM). No provider is called.",
            "index_rules": {"count": "protocol fixture: model calls in this process + agent replies recorded in the persisted log (full log)", "visible": "reply chosen from the tool results the model is shown (what a competent real model would do)"},
            "tools": "lookup, charge (ToolDefinition + ToolExecutor); ledger.jsonl outside the store, fsync before return",
            "idempotency_key": "not forwarded: ToolExecutor.__call__(action, conversation) receives no tool_call_id or key",
            "kill_points": "os.kill(getpid(), SIGKILL) inside the tool (W1, W2, W2c), inside the scripted model (W3), inside ConversationState.set_write_guard's guard around EventLog.append (W4a before, W4b after the charge observation file write), after run() returns WAITING_FOR_CONFIRMATION on charge (W6)",
            "paths": {"sdk": "LocalConversation(persistence_dir, conversation_id); resume = construct again + run()", "server": "openhands.agent_server.event_service.EventService in-process (start() = lease claim + stale-RUNNING recovery; run(); wait_for_run_completion()), no HTTP"},
        },
        "e1": {
            "success": e1["ledger"] == {"lookup": 1, "charge": 1, "charge_pids": e1["ledger"]["charge_pids"], "charge_amounts": [42]},
            "verified_by_ledger": f"{e1['ledger']['lookup']} lookup, {e1['ledger']['charge']} charge amount={e1['ledger']['charge_amounts']}",
            "model_calls": e1["model_calls"], "tool_calls": 2,
            "invalid_args_handled": "Scripted charge without `amount`: pydantic validation of ChargeAction failed in Agent._get_action_event, an AgentErrorEvent ('Error validating tool charge: ... amount Field required') went back to the model, the tool never ran; the next scripted reply was valid. " + f"{e1i['model_calls']} model calls, ledger {e1i['ledger']['lookup']} lookup + {e1i['ledger']['charge']} charge.",
            "retries": "1 (model-driven, after the validation error; no runtime retry)",
            "tokens": None, "wall_ms": e1["wall_ms"],
        },
        "e2": {
            "store": "LocalFileStore: one JSON file per event (atomic_write_text: temp file, fsync, os.replace; no directory fsync) + base_state.json (same writer); EventLog append under a file lock",
            "resume_api": "sdk: Conversation(agent, workspace, persistence_dir, conversation_id).run(); server: EventService(stored, conversations_dir, agent=None).start() then run()",
            "windows": windows,
        },
        "e3": {"supported": "partial", "notes": "Confirmation mode (AlwaysConfirm / ConfirmRisky) suspends with WAITING_FOR_CONFIRMATION; approval = run() (implicit confirmation of unmatched actions), rejection = reject_pending_actions() -> UserRejectObservation. W6 survives a restart. No handoff experiment (subagent delegation not exercised)."},
        "lease": lease,
        "e5": {"repetitions": e5["repetitions"], "in_memory_ms": e5["in_memory_ms"], "durable_ms": e5["durable_ms"], "import_s": e5["import_s"], "notes": "E1 task, LocalConversation, in-process; in_memory = persistence_dir None (InMemoryFileStore); durable = LocalFileStore. 1 warm-up each. import_s is a warm-cache import of the harness (litellm dominates)."},
        "blocked": ["E4: no local inference endpoint", "W5: handoff/subagent not exercised"],
    }
    (HERE / "results.json").write_text(json.dumps(res, indent=1) + "\n")
    for k, v in windows.items():
        print(f"{k:34s} {v['outcome']:17s} charges={v['charges_in_ledger']} consistent={v['final_state_consistent']} lost={v['progress_lost']}")


if __name__ == "__main__":
    main()
