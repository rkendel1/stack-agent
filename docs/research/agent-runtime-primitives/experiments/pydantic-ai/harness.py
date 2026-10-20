"""E1 / E2 / E6-ish harness for Pydantic AI + Pydantic AI Harness StepPersistence (FileStepStore).

Protocol: docs/research/agent-runtime-primitives/protocol.md. Scripted fake model (FunctionModel),
fake tools, side-effect ledger outside the store. No network model.

Usage (one phase per process; the driver script runs them):
  harness.py run    <workdir> <window>   # window in none,W1,W2,W3,W6,invalid
  harness.py resume <workdir> <window>
  harness.py report <workdir> <window>   # prints JSON facts for the window
"""

from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import time
from pathlib import Path

from pydantic_ai import Agent, DeferredToolRequests, DeferredToolResults
from pydantic_ai.messages import ModelMessage, ModelResponse, TextPart, ToolCallPart, ToolReturnPart, RetryPromptPart
from pydantic_ai.models.function import AgentInfo, FunctionModel
from pydantic_ai_harness.step_persistence import FileStepStore, StepPersistence
from pydantic_ai_harness.step_persistence.recovery import inspect_recovery

PRICE = 42
MODEL_CALLS = 0


def ledger_append(workdir: Path, tool: str, args: dict, invocation: str) -> None:
    line = json.dumps({'tool': tool, 'args': args, 'invocation': invocation, 'pid': os.getpid(), 't': time.time()})
    with open(workdir / 'ledger.jsonl', 'a', encoding='utf-8') as f:
        f.write(line + '\n')
        f.flush()
        os.fsync(f.fileno())


def die() -> None:
    os.kill(os.getpid(), signal.SIGKILL)


def build(workdir: Path, window: str, phase: str) -> tuple[Agent, FileStepStore]:
    store = FileStepStore(workdir / 'store')

    def script(messages: list[ModelMessage], info: AgentInfo) -> ModelResponse:
        # Fake model: the reply index is derived from the restored history (tool returns
        # already present), so a resumed process continues the same script.
        global MODEL_CALLS
        MODEL_CALLS += 1
        returns = [p for m in messages for p in getattr(m, 'parts', []) if isinstance(p, ToolReturnPart)]
        retries = [p for m in messages for p in getattr(m, 'parts', []) if isinstance(p, RetryPromptPart)]
        names = [r.tool_name for r in returns]
        if phase == 'run' and window == 'W3' and 'charge' in names:
            die()  # charge returned and its CallToolsNode boundary settled; die before next model step
        if 'lookup' not in names:
            return ModelResponse(parts=[ToolCallPart('lookup', {'key': 'price'}, tool_call_id='call-lookup')])
        if 'charge' not in names:
            if window == 'invalid' and not retries:
                return ModelResponse(parts=[ToolCallPart('charge', {'account': 'acct-7'}, tool_call_id='call-charge-bad')])
            return ModelResponse(
                parts=[ToolCallPart('charge', {'account': 'acct-7', 'amount': PRICE}, tool_call_id='call-charge')]
            )
        return ModelResponse(parts=[TextPart(f'Charged acct-7 {PRICE}.')])

    output_type = [str, DeferredToolRequests] if window == 'W6' else str
    agent = Agent(
        FunctionModel(script),
        output_type=output_type,
        capabilities=[StepPersistence(store=store, run_id=f'run-{window}-{phase}')],
    )

    @agent.tool_plain
    def lookup(key: str) -> int:
        ledger_append(workdir, 'lookup', {'key': key}, phase)
        return PRICE

    @agent.tool_plain(requires_approval=(window == 'W6'))
    def charge(account: str, amount: int) -> str:
        if phase == 'run' and window == 'W1':
            die()  # model chose charge, framework recorded `started`, effect not yet performed
        ledger_append(workdir, 'charge', {'account': account, 'amount': amount}, phase)
        if phase == 'run' and window == 'W2':
            die()  # effect fsynced in ledger, tool has not returned
        return f'charged {account} {amount}'

    return agent, store


async def phase_run(workdir: Path, window: str) -> dict:
    agent, store = build(workdir, window, 'run')
    t0 = time.perf_counter()
    result = await agent.run('Charge account acct-7 the amount lookup("price") returns, then report the charge.')
    out = {'output': repr(result.output), 'wall_ms': (time.perf_counter() - t0) * 1000, 'model_calls': MODEL_CALLS,
           'usage_requests': result.usage.requests, 'tool_calls': result.usage.tool_calls}
    if window == 'W6' and isinstance(result.output, DeferredToolRequests):
        (workdir / 'pending.json').write_text(json.dumps([c.tool_call_id for c in result.output.approvals]))
        # Persist the suspended history the way the docs recommend (Conversation / all_messages).
        from pydantic_ai.messages import ModelMessagesTypeAdapter
        (workdir / 'suspended_history.json').write_bytes(ModelMessagesTypeAdapter.dump_json(result.all_messages()))
        die()  # killed while suspended awaiting approval
    return out


async def phase_resume(workdir: Path, window: str) -> dict:
    agent, store = build(workdir, window, 'resume')
    run_id = f'run-{window}-run'
    insp = await inspect_recovery(store=store, run_id=run_id)
    facts = {
        'unresolved_tool_effects': [(r.tool_name, r.status, r.tool_call_id) for r in insp.unresolved],
        'latest_snapshot_state': getattr(insp.latest, 'state', None),
        'settled_snapshot_exists': insp.settled is not None,
        'latest_snapshot_messages': len(insp.latest.messages) if insp.latest else 0,
    }
    if window == 'W6':
        from pydantic_ai.messages import ModelMessagesTypeAdapter
        hist = ModelMessagesTypeAdapter.validate_json((workdir / 'suspended_history.json').read_bytes())
        ids = json.loads((workdir / 'pending.json').read_text())
        res = await agent.run(message_history=hist, deferred_tool_results=DeferredToolResults(approvals={i: True for i in ids}))
        facts['output'] = repr(res.output)
        return facts
    if insp.latest is None:
        facts['resume_error'] = 'no snapshot to continue from'
        return facts
    # Documented continuation: latest snapshot (include_interrupted) -> Agent.run(message_history=...).
    hist = list(insp.latest.messages)
    try:
        res = await agent.run(message_history=hist)
        facts['output'] = repr(res.output)
    except Exception as e:  # noqa: BLE001
        facts['resume_error'] = f'{type(e).__name__}: {e}'[:400]
    facts['model_calls_in_resume'] = MODEL_CALLS
    return facts


def main() -> None:
    cmd, workdir, window = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
    workdir.mkdir(parents=True, exist_ok=True)
    fn = {'run': phase_run, 'resume': phase_resume}[cmd]
    out = asyncio.run(fn(workdir, window))
    (workdir / f'{cmd}.json').write_text(json.dumps(out, indent=2, default=str))
    print(json.dumps(out, default=str))


if __name__ == '__main__':
    main()
