# smolagents experiments

Harness for the shared experiments in [`../../protocol.md`](../../protocol.md), run against
smolagents. Dossier: [`../../candidates/smolagents.md`](../../candidates/smolagents.md).

* Source inspected: `huggingface/smolagents` at `96f33faaf028479119ec8d34507b47694cf14e34`
  (cloned with `--depth 1`, not executed).
* Package used: `smolagents==1.26.0` from PyPI. Its Python sources are byte-identical to the
  inspected commit except `__init__.py` (version string `1.27.0.dev0` in the source tree) and two
  unreferenced leftover files shipped in the wheel (`tmp.py`, `pyodide_deno_executor.bak.py`).
* Fake model: `ScriptedModel(smolagents.models.Model)` returns `ChatMessage`s with native tool calls,
  JSON-in-text tool calls, or `<code>` blobs. Replies are indexed by calls in this process (smolagents
  restores no state, so the restored-replies term is always 0). The charge amount is read from the
  observation the model was shown, not hardcoded.
* Tools: `lookup` and `charge` are `smolagents.Tool` subclasses that append to and `fsync` a
  per-case `logs/*.ledger.jsonl` before returning. smolagents has no idempotency key to forward.
* No network access is used. E4 is blocked: there is no local inference endpoint.

## Commands

```sh
R=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research
python3 -m venv $R/venv/smolagents
$R/venv/smolagents/bin/pip install smolagents          # resolved to 1.26.0
cd /home/user/stack-agent-research/docs/research/agent-runtime-primitives/experiments/smolagents
$R/venv/smolagents/bin/python -I harness.py all > logs/run_all.stdout.txt   # writes results.json + logs/
# or one experiment at a time: e1 | e2 | e3 | e5
```

The whole run takes about 15 s on this machine (4 CPUs, Python 3.13.16, Linux x86_64).

## What each experiment does

| Exp | Cases |
| --- | --- |
| E1 | `tc_native` (ToolCallingAgent, native tool calls), `tc_text_toolcall` (JSON tool call in plain text, parsed by `Model.parse_tool_calls`), `tc_missing_arg`, `tc_wrong_type`, `tc_bad_json`, `tc_final_answer_check` (premature final answer rejected by a `final_answer_checks` gate), `tc_max_steps` (`max_steps=2`, never finishes), `code_3step` (CodeAgent, one tool per blob), `code_1blob` (whole task in one blob), `code_missing_arg`, `code_wrong_type`, `code_partial_effect_retry` (the blob charges and then fails; the scripted model re-sends the blob), `code_no_code_block` |
| E2 | Each child process runs the E1 task with ToolCallingAgent and `SIGKILL`s itself at W1 (on the streamed `ToolCall` for `charge`), W2 (inside `charge`, after the ledger fsync) or W3 (in a step callback after the charge step), or at W5 (on the manager's streamed `ToolCall` to the managed `specialist`). A fresh process then "resumes". smolagents has no store and no resume API, so the only built-in option is to re-run the task. A user-written step callback dumps memory to JSON (fsync + rename) to show what D1 a user can build. W4 and W6 are `not_applicable`. |
| E3 | Manager to managed `specialist` delegation (captures the context the specialist receives and what the manager gets back), a specialist whose model raises, an approval attempt via step callback + `agent.interrupt()`, and an approval attempt by stopping the `run(stream=True)` generator at the `charge` `ToolCall` |
| E5 | 1 warm-up + N=50 runs of agent construction + E1 run for ToolCallingAgent, CodeAgent (3 blobs) and CodeAgent (1 blob); `import smolagents` time in 5 fresh interpreters. In-memory only (there is no durable store). |

## Results (from `results.json`)

| Case | verified | model calls | ledger tool calls | error steps | note |
| --- | --- | --- | --- | --- | --- |
| tc_native | yes | 3 | 2 | 0 | |
| tc_text_toolcall | yes | 3 | 2 | 0 | JSON in text parsed with first `{` to last `}` |
| tc_missing_arg | yes | 4 | 2 | 1 | `AgentToolCallError: Argument amount is required` fed back as observation |
| tc_wrong_type | yes | 4 | 2 | 1 | `Argument amount has type 'string' but should be 'number'` |
| tc_bad_json | yes | 4 | 2 | 1 | `AgentParsingError` with JSON decode position fed back |
| tc_final_answer_check | yes | 4 | 2 | 1 | premature final answer refused; the check's message is fed back |
| tc_max_steps | **no** | 3 | 2 (both `lookup`, 0 charges) | 1 | an extra tool-less model call produces the answer; `run()` returns the made-up "I think the account was charged 12.5." (`RunResult.state="max_steps_error"` only when `return_full_result=True`) |
| code_3step | yes | 3 | 2 | 0 | `price` carries between blobs in the interpreter state |
| code_1blob | yes | **1** | 2 | 0 | whole task in one model call |
| code_missing_arg | yes | 4 | 2 | 1 | Python `TypeError` from `forward()` surfaced |
| code_wrong_type | **no** | 3 | 2 | 0 | **no schema validation in the code path**: `charge(amount="twelve")` executed |
| code_partial_effect_retry | **no** | 2 | 4 (**2 charges**) | 1 | the error observation does not mention that the charge already happened |
| code_no_code_block | yes | 2 | 2 | 1 | regex parse error fed back |

E2: W1 `lost_progress` (the lookup is redone, 1 charge), W2 `duplicate_effect` (2 charges), W3
`duplicate_effect` (2 charges), W5 `correct` only because nothing had run before the kill (the
whole delegation was re-run), W4 and W6 `not_applicable`. `resumed` is false everywhere.

E3: the specialist receives only the task string wrapped in a "managed agent" template (no manager
history). The manager receives only the specialist's final answer string. A specialist failure
becomes an error observation for the manager. The step callback fires *after* `charge` executed,
so `interrupt()` cannot gate it. Closing the stream generator at the `charge` `ToolCall` does
prevent the charge, but it raises `RuntimeError: generator ignored GeneratorExit`, and the run
cannot be continued.

E5 (median, p10–p90, ms per run): ToolCallingAgent about 16 (15–19), CodeAgent 3-blob about 27
(25–42), CodeAgent 1-blob about 26 (24–36); `import smolagents` about 490 ms. These numbers are
comparable only with other harnesses on this machine.
