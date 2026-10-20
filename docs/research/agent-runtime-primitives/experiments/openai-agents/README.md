# openai-agents experiments (E1, E2, E3, E5)

Candidate: OpenAI Agents SDK (Python), `openai-agents==0.24.0`, source inspected at
[`64cd5df`](https://github.com/openai/openai-agents-python/tree/64cd5dfc5a1579870dcbed0ea42b27aa84033fee).
E4 is blocked (no local inference endpoint).

## What the harness does

`harness.py` uses only the SDK's public API plus one documented extension point (subclassing
`agents.models.interface.Model`):

* **Fake model** `FakeModel(Model)`: returns scripted `ModelResponse`s (function calls / one
  assistant message), reporting `Usage(requests=1, input=10, output=5)`. Reply index = calls in this
  process + replies already recorded in restored state (counted from `SQLiteSession` items, or from
  `model_responses` in a serialized `RunState`).
* **No network**: `OPENAI_AGENTS_DISABLE_TRACING=1`, `set_tracing_disabled(True)`, a dummy
  `OPENAI_API_KEY`; every agent is given the fake model instance directly.
* **Ledger**: `ledger.jsonl` in the run's work dir (outside the SDK's `session.db` / `state.json`),
  one fsynced line per tool invocation before the tool returns.
* **Kill points** (all `os.kill(getpid(), SIGKILL)`, no timing):
  * W1 `RunHooks.on_tool_start` for `charge` (model chose it, body not run);
  * W2 inside `charge`, after the ledger fsync;
  * W3 at the start of the next model call (turn's items already committed to SQLite);
  * W4 in a `SQLiteSession` subclass's `add_items`, before `super().add_items` for the batch holding
    the charge output (between "about to persist" and "persisted");
  * W5 `RunHooks.on_handoff` (Triage -> Specialist);
  * W6 after the approval interruption was returned and `RunState.to_string()` was written
    (atomic rename + fsync) — i.e. while suspended.
  * Extra `W2_after_approval`: W2 inside the approved `charge` during the resumed run.
* **Resume**: W1–W5 use `SQLiteSession` (file DB) and a fresh `Runner.run(agent, "continue",
  session=...)` — the SDK has no API to resume an un-interrupted crashed run. W6 /
  W2_after_approval use `RunState.from_string(...)`, `state.approve(...)`, `Runner.run(agent, state,
  session=...)`.

## Exact commands

```bash
R=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research
python3 -m venv $R/venv/openai-agents
$R/venv/openai-agents/bin/pip install openai-agents          # resolved 0.24.0
cd /tmp && $R/venv/openai-agents/bin/python -I \
  /home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/openai-agents/harness.py \
  all --work $R/runs/openai-agents \
  --out /home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/openai-agents/results.json
```

`all` spawns one subprocess per phase (`e1`, `e2-run`, `e2-resume`, `e3-p1`, `e3-p2`,
`e3-session-resume`, `e3-fail`, `e5`) so every resume is a fresh process. Full run ≈ 75 s, dominated
by ~2.4 s SDK import per subprocess.

## Outputs

* `results.json` — protocol shape plus per-window `resume_detail` (session items before/after).
* `logs/<phase>.events.log` — pid-stamped markers (model calls, tool starts, session writes, kills).
* `logs/<phase>.ledger.jsonl` — the side-effect ledgers (E5 logs omitted: 150+ repetitive runs).

Environment of the recorded run: 4 CPUs (see `results.json.environment`), Python 3.13.16, Linux.
