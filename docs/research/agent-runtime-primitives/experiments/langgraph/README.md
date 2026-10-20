# LangGraph experiments (E1, E2, E3, E5)

Candidate commit `1400d25b7b88cfa055c55b8a82bd95b7c25caef1`. The experiments use the PyPI packages, whose versions
match the `pyproject.toml` files at that commit: `langgraph==1.2.16`, `langgraph-checkpoint==4.3.0`,
`langgraph-checkpoint-sqlite==3.1.2`, `langgraph-prebuilt==1.1.1`, `langchain-core==1.6.12`. The Python is CPython
3.13.16 on Linux with 4 CPUs. E4 is blocked because no local model is available.

## Fixture

* **Fake model.** `common.ScriptedChatModel` is a `langchain_core` `BaseChatModel` subclass. It replays a script of
  `AIMessage`s that carry tool calls. Reply index = AI replies in the restored state + calls in this process. It is
  a custom class because langchain's `GenericFakeChatModel` cannot emit tool calls through `bind_tools`, and a custom
  `BaseChatModel` exercises the same interface a real provider adapter uses.
* **Tools.** `lookup` and `charge` are `@tool` functions run by `langgraph.prebuilt.ToolNode`. Each one appends a
  ledger line and fsyncs it before returning. The line records `task_id`, `checkpoint_id` and `tool_call_id` from the
  injected `ToolRuntime`. These are candidate idempotency keys. LangGraph does not forward or check any key itself.
* **Kill hooks.** `common.KillingSqliteSaver` subclasses `SqliteSaver`. It overrides `cursor`, `put` and `put_writes`
  only to `SIGKILL` the process at named points. `W4`/`W4b` kill after the SQL ran but before `COMMIT`. `W1` and `W2`
  sit in the `charge` tool, `W3b` in the model, and `W6` in `e3_child.py` after `invoke` returns suspended. The env var
  `KILL_AT` selects the hook. Kill points are reached deterministically, never by timing.
* **Ground truth.** `ledger.jsonl` lives outside the sqlite store.

## Commands

```sh
S=<scratch>/research; V=$S/venv/langgraph/bin/python; X=<this dir>
python3 -m venv $S/venv/langgraph
$S/venv/langgraph/bin/pip install --no-cache-dir langgraph langgraph-checkpoint-sqlite langchain-core
$V -I $X/e2.py $S/work sync async exit        # E2 W1..W4b, 5 reps per window per durability mode
$V -I $X/e2_concurrent.py $S/work             # E2 supplement: two resumers, no fence
$V -I $X/e3.py $S/work                        # E3 + E2 W5/W6
$V -I $X/e1_e5.py $S/work                     # E1 + E5 (run alone on an idle machine)
$V -I $X/assemble.py $S/work 1400d25b7b88cfa055c55b8a82bd95b7c25caef1   # -> results.json, raw/
```

`raw/` holds the merged per-experiment JSON. The per-window sqlite files and ledgers stay in the scratch work
directory. E2's resume API is `graph.invoke(None, {"configurable": {"thread_id": ...}})` in a fresh process.
