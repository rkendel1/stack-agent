# OpenHands software-agent-sdk experiment

The decisive experiment the Tier 3 screen left open: E2 under real `SIGKILL`, resumed from the
persisted event log. It runs two ways: through the bare SDK's `LocalConversation`, and through the
agent server's `EventService.start()` recovery rule. It also covers E1 and E5, and a direct test of
the agent server's owner lease.

## Setup

| | |
| --- | --- |
| Source | `OpenHands/software-agent-sdk@33d8aa29c3078d074799e2fbf5e847aeb2d71360` |
| Packages | `openhands-sdk==1.56.0`, `openhands-agent-server==1.56.0`, `openhands-tools==1.56.0`, from PyPI, in a venv outside the repo (`uv pip`, no cache). The SDK, server and lease modules the harness exercises are byte-identical to the pinned source (checked with `cmp`) |
| Machine | 4 CPUs, Python 3.13.16, Linux 6.18.44 |

## Fixture

* **Model.** `ScriptedLLM` subclasses the SDK's own `openhands.sdk.testing.TestLLM`.
  * The agent server re-validates the agent on start and loads it from `base_state.json` on
    resume, and both turn the subclass back into a plain `LLM`. So the harness also routes
    `LLM.completion` and `LLM.acompletion` of every instance to the script. No provider is
    ever called.
  * The `count` index rule is the protocol fixture: model calls in this process, plus agent replies
    already recorded in the persisted log.
  * The `visible` index rule chooses the reply from the tool results the model is actually shown.
    It is reported beside `count` because the SDK can show the model a history that differs from
    the log (see below).
* **Tools.** `lookup` and `charge` are `ToolDefinition` + `ToolExecutor`.
  * Each appends to `ledger.jsonl`, outside the store, and fsyncs before returning.
  * The executor's signature is `__call__(action, conversation)`. No tool-call id or idempotency
    key reaches it.
* **Kill points.** Each is a deterministic `os.kill(os.getpid(), SIGKILL)`:

  | Window | Kill point |
  | --- | --- |
  | W1 | charge entry, before the ledger write |
  | W2 | inside charge, after the ledger fsync |
  | W2c | as W2, but in confirmation mode, after the person approved charge |
  | W3 | start of model call 3 |
  | W4a | in `ConversationState.set_write_guard`'s guard, before the charge `ObservationEvent` file is written |
  | W4b | after that file is written, before the index, length marker and HEAD advance |
  | W6 | `AlwaysConfirm`, after `run()` returns `WAITING_FOR_CONFIRMATION` on charge |

* **Resume.**
  * `sdk`: construct `Conversation(agent, workspace, persistence_dir, conversation_id)` again
    and call `run()`, the documented pattern in `examples/01_standalone_sdk/10_persistence.py`.
    Approval is `run()`, and rejection is `reject_pending_actions()`.
  * `server`: `EventService(stored, conversations_dir, agent=None).start()`, which claims the
    lease and applies the stale-RUNNING rule, then `run()`. Accepting a confirmation is `run()`,
    as `respond_to_confirmation(accept=True)` does.

## Run

```
python -I driver.py <venv-python> <runs-dir> logs [case ...]   # E2 (each case: run -> SIGKILL, resume)
python -I harness.py e1 <dir> [invalid]                          # E1
python -I harness.py e5 <dir> 50                                 # E5
python -I harness.py lease <dir>                                 # lease takeover/fencing
python3 -I assemble.py <runs-dir>                                # results.json from logs/ + run dirs
```

`logs/` holds the raw per-case records: trace, ledger, and the persisted state before and after
the resume. `results.json` follows `protocol.md`.

## Results

| Window | bare SDK (`count` / `visible`) | agent server (`count` / `visible`) |
| --- | --- | --- |
| W1 | lost_progress (0) / correct (1) | lost_progress (0) / lost_progress (0) |
| W2 | correct (1, state ≠ ledger) / **duplicate_effect (2)** | correct (1, record says "did not complete") / correct (1) |
| W2c (approved, then crash) | **duplicate_effect (2)** / **duplicate_effect (2)** | **duplicate_effect (2)** / **duplicate_effect (2)** |
| W3 | correct / correct | correct / correct |
| W4a | correct (1, state ≠ ledger) / **duplicate_effect (2)** | correct / correct |
| W4b | correct (1, state ≠ ledger) / **duplicate_effect (2)** | correct (1, state ≠ ledger) / **duplicate_effect (2)** |
| W6 | correct (approve) / correct; reject: correct (0) | correct / correct |
| sensitivity | — | a model that re-issues after the restart error: W1 correct (1), W2 duplicate (2) |

Findings:

1. **The bare SDK does not re-run the interrupted tool through implicit confirmation in normal
   mode.**
   * HEAD (`leaf_event_id` in `base_state.json`) is saved only when the step's `with state:`
     block exits. So every event of the interrupted step is fsynced but off the active branch.
   * The resumed conversation never sees the pending action. The model is asked again from the
     pre-step history, and a model that re-decides charge duplicates it (`visible`).
   * The orphan `ActionEvent` stays unmatched in the full log.
2. **The agent server's rule** (stale RUNNING becomes ERROR, then a full-log scan and a synthetic
   error parented to the action) repairs W1, W2 and W4a: no re-execution.
   * It turns the ambiguity into a false negative ("did not complete").
   * It cannot tell W1 from W2.
   * It misses W4b, where the observation is on disk but off-branch.
3. **Confirmation mode has a re-execution window in both paths (W2c).**
   * `run()` flips `WAITING_FOR_CONFIRMATION → RUNNING` inside the same deferred-save block as the
     step. A crash inside the approved tool therefore leaves the status persisted as still
     waiting.
   * The stale-RUNNING rule never fires, and approving again re-executes the charge.
4. **Lease** (`logs/lease.json`).
   * A live owner refuses a second claimant (`ConversationLeaseHeldError`).
   * After expiry, the takeover gets generation 2, and the old owner's `guarded_write` and `renew`
     are refused (`ConversationOwnershipLostError`).
   * In E2 the takeover after a dead PID on the same host was immediate (generation 1 → 2).
   * A corrupt lease file is treated as absent, and the generation restarts at 1.

E1: verified, 3 model calls, 1 lookup + 1 charge (42). A charge without `amount` fails pydantic
validation before the tool runs, and the model sees an `AgentErrorEvent`.

E5 (50 repetitions after 1 warm-up, E1 task in-process):

| Store | p10 | median | p90 |
| --- | --- | --- | --- |
| in-memory | 16.3 ms | 18.2 ms | 27.5 ms |
| `LocalFileStore` (durable) | 36.5 ms | 41.7 ms | 54.9 ms |

Import took 5.3 s with a warm cache. litellm dominates it.
