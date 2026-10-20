# MAF (Python) E1/E2 crash harness

Candidate: Microsoft Agent Framework, commit `809cc04f2d610bb48fa4274b6e7f0e96773d576d`,
package `agent-framework-core==1.22.0` (same version as `python/packages/core/pyproject.toml` at that commit).

What it tests: a graph workflow (`WorkflowBuilder`) with two executors, `model` (scripted fake model,
indexed by replies already present in restored executor state) and `tools` (`lookup` / `charge`,
each appending one fsynced line to `ledger.jsonl` outside the checkpoint store). Durable store:
`FileCheckpointStorage` (the most durable built-in Python option in core). The first process
`SIGKILL`s itself at an exact point; a fresh process calls `storage.get_latest(...)` and
`workflow.run(checkpoint_id=..., [responses=...])`. No model, no network.

Windows: W1 (charge chosen, not invoked), W2 (charge ledger line fsynced, tool not returned),
W3 (charge returned and its superstep checkpoint saved), W4 (inside `FileCheckpointStorage.save`
of the checkpoint that would record the charge, before the write; hook = public-API subclass),
W6 (`ctx.request_info` approval pending, IDLE_WITH_PENDING_REQUESTS checkpoint written, then kill;
resume passes `responses={request_id: True}`). W5 not run (handoff needs a chat-client agent).

## Commands

```bash
VENV=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research/venv/maf
uv venv $VENV && VIRTUAL_ENV=$VENV uv pip install agent-framework-core==1.22.0
RUN=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research/run/maf
mkdir -p $RUN && cd $RUN
$VENV/bin/python -I /home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/maf/driver.py $VENV/bin/python $RUN
```

`driver.py` runs E1 once, then each window in a fresh workdir (`run` process, then `resume`
process), and writes `results.json` next to itself.

## Result (2026-10-20, this machine)

| Window | charges in ledger | outcome |
| --- | --- | --- |
| W1 | 1 | correct |
| W2 | 2 | duplicate_effect |
| W3 | 1 | correct |
| W4 | 2 | duplicate_effect |
| W6 | 1 | correct (pending approval survived restart) |

Resumption needed no manual reconstruction in any window. Duplicates in W2/W4 follow from the design:
the checkpoint is written only at the superstep boundary *after* the executor ran, nothing is
recorded before the effect, and no idempotency key is offered to the tool.
