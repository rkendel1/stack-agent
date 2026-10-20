# CrewAI Flow E1/E2 crash harness

Candidate: CrewAI, commit `c6e500ab3730ccf3badb640c6e0170e5c6aa3882`, package `crewai==1.15.28`
(`__version__` at that commit). Public API only; no LLM, no Agent/Crew: the flow is five methods
`decide_lookup -> do_lookup -> decide_charge -> do_charge -> report` (plus `approve_charge` with
`@human_feedback` in the approval variant). Tools append one fsynced line to `ledger.jsonl` outside
every CrewAI store.

Mechanisms (each the candidate's documented resume API):

* `persist`: class-level `@persist(SQLiteFlowPersistence(db))`; resume = new instance,
  `kickoff(inputs={"id": <flow id>})`. W4 hook = public-API subclass of `SQLiteFlowPersistence`
  that kills inside `save_state` for `do_charge` before the write.
* `ckpt`: `CheckpointConfig(location=db, provider=SqliteProvider(), on_events=["method_execution_finished"])`;
  resume = `kickoff(from_checkpoint=CheckpointConfig(..., restore_from="<db>#<latest id>"))`, with the
  latest id read from the provider's `checkpoints` table by rowid. Each ckpt window ran 3 times
  because checkpoint writes are dispatched on the event bus thread pool.
* `approval`: `@persist` + `@human_feedback(provider=...)` whose provider raises
  `HumanFeedbackPending`; kill after `kickoff()` returns the pending object; resume =
  `Flow.from_pending(id, persistence).resume("approved")`.

Deviation from the shared fixture: the fake model is keyed by the calling decision step, not by
the number of recorded replies. With reply-count indexing the first attempt crashed on every
`@persist` resume, because `@persist` re-runs from `@start` against restored state that already
holds later replies. That crash was a harness artifact and hid the effect behaviour, so the
fixture was changed.

## Commands

```bash
VENV=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research/venv/crewai
uv venv $VENV && VIRTUAL_ENV=$VENV uv pip install --no-cache crewai==1.15.28
RUN=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research/run/crewai
mkdir -p $RUN && cd $RUN
python3 -I /home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/crewai/driver.py $VENV/bin/python $RUN
```

The driver sets `CREWAI_DISABLE_TELEMETRY=true`, `OTEL_SDK_DISABLED=true`, `CREWAI_TRACING_ENABLED=false`
and `HOME=<workdir>` for each child process, then writes `results.json` next to itself.

## Result (2026-10-20, this machine)

| Window | charges | lookups | outcome |
| --- | --- | --- | --- |
| persist/W1 | 1 | 2 | lost_progress (whole flow re-ran from `@start`) |
| persist/W2 | 2 | 2 | duplicate_effect |
| persist/W3 | 2 | 2 | duplicate_effect (even after `do_charge`'s state row was committed) |
| persist/W4 | 2 | 2 | duplicate_effect |
| ckpt/W1 x3 | 0 | 1 | stuck: no checkpoint row had landed although 3 methods had finished |
| ckpt/W2 x3 | 2 / 1 / 1 | 1 | duplicate_effect once, stuck twice (no row) |
| ckpt/W3 x3 | 2 / 1 / 2 | 1 | duplicate_effect twice, stuck once (no row) |
| approval/W6 | 1 | 1 | correct: the pending feedback survived `SIGKILL` and `from_pending().resume()` continued |

A clean (unkilled) `ckpt` run writes 5 checkpoint rows, so the missing rows after a kill are
write lag, not a configuration error: checkpoint writes are not ordered before the next method runs.
