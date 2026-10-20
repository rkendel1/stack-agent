# Letta experiments

Results: [`results.json`](results.json). Dossier: [`../../candidates/letta.md`](../../candidates/letta.md).

## What was run, and on which Letta

`letta-ai/letta` `main` (8816d3a8) is now a landing page. The current implementation is
`letta-ai/letta-code` (TypeScript), and PyPI `letta` now ships it: a wheel with the `letta` CLI and
its own bundled Node. The Python V1 server that matches the brief (Postgres/SQLite, `core_memory_*`
tools, heartbeats) is kept on the `archive` branch (56ba9c25) and is marked retired. Its old
packages and Docker image are unsupported.

* **Run:** `letta==0.34.12` from PyPI, installed in `scratchpad/research/venv/letta`, using the
  **local backend** (`--backend local`, file store under `~/.letta/lc-local-backend`) and headless
  mode (`-p ... --output-format json`). The source inspected is letta-code `e8073d4e` (0.34.18).
  Those are six patch releases apart, so behavior could differ in places.
* **Not run:** the V1 server. It is retired upstream, so any numbers from it would describe a
  system that no longer exists. It was inspected as source only.

## Harness

* `fake_openai.py` is a scripted, stdlib-only OpenAI-compatible Chat Completions server
  (`/v1/models`, streaming `/v1/chat/completions`). It is registered with
  `letta connect openai-compatible --base-url http://127.0.0.1:<port>/v1`, and the harness uses the
  model `openai-compatible/fake`. Each reply depends on the tool results already in the request:
  none → `lookup`, lookup result → `charge`, charge result → final answer. That makes it
  "indexed by replies already recorded in restored state", as the protocol asks. `MODE=omit_arg`
  makes the first call leave out the required `command` argument.
* `tool.py` holds the fake tools. Letta Code has no plain custom-function tools in headless mode
  without mods or MCP, so the scripted model calls the **built-in `Bash` tool**, which runs
  `python3 -I tool.py lookup|charge`. Each call appends to `ledger.jsonl` and fsyncs before it
  returns. No idempotency key exists to forward.
* The kill points are deterministic: SIGKILL goes to the whole `letta` process group (the Python
  wrapper and its Node child).
  * W1: inside `charge`, before the ledger write.
  * W2: inside `charge`, after the ledger fsync, before the tool returns.
  * W3: the fake server kills when it receives the request that carries the charge result, so
    Letta has already appended that result to `messages.jsonl`.
* Resume uses the CLI's documented way to continue a conversation:
  `letta --backend local -p "Continue the task." --conversation local-conv-1`. `--agent` and
  `--conversation` cannot be combined.
* `harness.py` runs E1, E2 (W1–W3) and E5. Each scenario gets its own `HOME`. Run it with
  `python3 -I harness.py <venv>/bin/letta <workdir> results.json`, and set `ONLY=e1,e2,e5` to
  pick sections.

## Results (this machine: 4 CPUs, Linux x86_64)

| | Result |
| --- | --- |
| E1 | **Verified.** The ledger shows exactly 1 lookup and 1 charge(42). 3 model calls and 2 tool calls. Letta reported `step_count: 0`. Wall time was about 3.9 s for the whole CLI process. |
| E1, missing argument | The Bash tool returned `Bash tool missing required parameter: command. Received parameters: description` as a tool result, and the loop went on (4 model calls; the ledger was still correct). |
| E2 W1 | `correct`. The pending charge call was closed on resume with a synthetic "stale pending tool call … no result was ever recorded … Re-issue the tool call if you still need it" result. The model issued the charge again; the first attempt never reached the effect, so the ledger holds one charge. |
| E2 W2 | **`duplicate_effect`, 2 charges.** The effect had happened, but the resumed run told the model the same thing as in W1, that no result was ever recorded and it should re-issue. The scripted model re-issued, and the charge ran a second time. |
| E2 W3 | `correct`. The result was already on disk, and resume produced the final answer. |
| E2 W4 | `not_applicable`. There is no hook inside the store write; the writes are synchronous `appendFileSync`/`writeFileSync` with no fsync. |
| E2 W5/W6, E3 | Not measured; see the blocked list. |
| E5 | 50 runs. Each run is a full `letta -p` process on the durable local store: p10 2686 ms, median 2980 ms, p90 3333 ms. `letta --version` cold start: median 1693 ms. No in-process API exists, so these numbers include Node startup and are not comparable to in-process harnesses. |

## Blocked or not done

* **E3, W5, W6:** Durable approval waits and the pre-effect fence live in the websocket listener
  path (`letta server` / App Server, `src/websocket/listener/*`). Headless `-p` defaults to the
  `unrestricted` permission mode and never suspends. Driving the listener protocol with a scripted
  client was beyond this session's time budget. Source analysis is in the dossier.
* **E4:** No local inference endpoint is available (see the protocol).
* **V1 server:** Not run; see above.

## Caveats

* Telemetry was left on (`LETTA_CODE_TELEM` and `DO_NOT_TRACK` were not set). Model traffic went
  only to the local fake. Whether telemetry left the machine was not checked.
* The W1 and W2 outcomes depend on what the model does after it receives the synthetic
  "re-issue if you still need it" result. The scripted model always re-issues. A real model might
  check first, but nothing in Letta's headless path guarantees that.
