# Agno experiments (E1, E2, E3, E5)

Harness: `agno_harness.py`. It uses only Agno's public API, a scripted fake `Model` subclass
(`ScriptedModel`), and the fake tools `lookup` and `charge`, which append to and fsync
`ledger.jsonl`. It makes no network calls. Telemetry is off (`AGNO_TELEMETRY=false` and `telemetry=False`).

* Source: `https://github.com/agno-agi/agno` at `643f6e797312bb00a80a7bc2e445449b8106970d` (depth-1 clone, never executed).
* Package: `agno==3.1.6` from PyPI, `SQLAlchemy==2.1.5`, `greenlet==3.5.6` (`agno.db.sqlite` imports
  `sqlalchemy.ext.asyncio`, which fails without greenlet), `pydantic==2.14.1`.
* Machine: 4 vCPU Linux VM (kernel 6.18.44, x86_64) running Python 3.13.16.

## Setup

```bash
R=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research
H=/home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/agno/agno_harness.py
python3 -m venv $R/venv/agno
$R/venv/agno/bin/pip install --no-cache-dir agno sqlalchemy greenlet
mkdir -p $R/run/agno/e5 && cd $R/run/agno
```

## Commands

```bash
export AGNO_TELEMETRY=false
$R/venv/agno/bin/python -I $H e1 --out $R/run/agno            # prints E1 JSON
$R/venv/agno/bin/python -I $H e2 --out $R/run/agno            # writes e2_raw.json; spawns child/resume processes
$R/venv/agno/bin/python -I $H e3 --out $R/run/agno            # writes e3_raw.json
$R/venv/agno/bin/python -I $H e5 --out $R/run/agno/e5 --n 50  # prints E5 JSON
```

The raw outputs are copied into `logs/`: `e1_raw.json`, `e2_raw.json`, `e3_raw.json`, `e5_raw.json`, plus one
ledger per E2 window (`ledger_<checkpoint>_<window>.jsonl`). `results.json` is built from these files.

## How the kill points are reached (deterministic, `SIGKILL` to self)

| Window | Mechanism |
| --- | --- |
| W1 | Agent-level `tool_hooks=[kill_hook]`. The hook runs after the model chose `charge` and before the entrypoint is called. |
| W2 | Inside `charge`, after the ledger line is fsynced and before the tool returns. |
| W3 | In the fake model's third call. That call happens only after the `after_tool_results` checkpoint for the charge batch has returned (`checkpoint="tool-batch"`). |
| W4 | SQLAlchemy `commit` engine event (fires before COMMIT is sent). Kill at the 1st (`commit1`, session row) or the 2nd (`commit2`, run row) commit after the charge ledger line exists. |
| W5 | In the Specialist's first model call: the Triage leader has issued `delegate_task_to_member` and the Specialist has not acted yet (E3 harness). |
| W6 | `charge` has `@tool(requires_confirmation=True)`. The run returns PAUSED (persisted), then the process kills itself. |
| W6b | Same as W6, but the run is approved in-process, `continue_run` is called, and the process dies inside the approved `charge` after the ledger write. |

Resume always happens in a fresh process: `Agent.get_run_output(run_id, session_id)` loads the run.
Unresolved requirements are confirmed, then `Agent.continue_run(run_id=..., session_id=..., requirements=...)`
is called. When there is no stored run, the harness records `resumed=false` and resubmits the task as a new
run, which counts as manual intervention.

E2 runs twice: once with `checkpoint="tool-batch"` (the most durable built-in agent option) and once with
the default `checkpoint="runs"`.

## Fake model indexing

The reply index is the number of assistant messages in the conversation Agno passes to `invoke`. That is the
replies made in this process plus the replies already recorded in restored state, so a resumed run
continues the same script. Tool-call ids are random per call, as real providers produce them.
