# Research protocol: agent-runtime primitives

Every investigation in this directory follows this protocol, so findings and experiment results
are comparable across candidates. This is research only: nothing here changes stack-agent's code,
dependencies or behavior.

## Evidence classes

Every substantive finding carries exactly one tag:

| Tag | Meaning |
| --- | --- |
| `[source]` | Verified in source: a specific file, function or test at the pinned commit. Cite `path:line` and the commit link. |
| `[experiment]` | Verified experimentally: reproduced by a harness in `experiments/` with a recorded result. |
| `[claim]` | Stated by the project's documentation, not reproduced. |
| `[inferred]` | A reasoned interpretation, not verified. |
| `[blocked]` | Could not be evaluated here; the reason is stated. |

Star counts, download counts and marketing benchmarks are not evidence of runtime quality.
Performance numbers are compared only under equivalent conditions (same machine, model, task
and repetitions).

## Durability is four properties, never one

Each candidate is judged on each property separately:

| | Property | Question |
| --- | --- | --- |
| D1 | **Save** | Can state be written somewhere that survives the process? |
| D2 | **Resume** | Can a new process continue the same execution from the saved state, without manual reconstruction? |
| D3 | **Safe recovery** | After a crash at any boundary, does resumption neither lose nor corrupt recorded progress? |
| D4 | **Effect safety** | Are external side effects protected against unsafe repetition after a crash (idempotency keys, intents recorded before effects, ambiguous outcomes surfaced)? |

"Persists sessions" establishes D1 at most. Exactly-once is never claimed without evidence of how
external effects are protected.

## Candidate record

Each candidate's dossier (`candidates/<name>.md`) starts with this record:

* repository URL, verified as the project's canonical repository;
* inspected commit SHA, and an immutable link `https://github.com/<owner>/<repo>/tree/<sha>`;
* license (from the LICENSE file at that commit);
* last meaningful maintenance activity (date and subject of the latest non-trivial commit);
* installed package and version used for experiments, if any.

Source references use immutable links: `https://github.com/<owner>/<repo>/blob/<sha>/<path>#L<n>`.

## Shared experiments

Harnesses live in `experiments/<candidate>/` and write `results.json`. A harness uses only the
candidate's public API, a **scripted fake model** and **fake tools**. No network model is
called. Real inference is required only by E4.

### Shared fixture

* **Fake model.** A deterministic script of replies, indexed by how many times the model has been
  called in this process plus the number of replies already recorded in restored state. If a
  candidate cannot accept a custom model, the harness uses the candidate's own test or fake model
  class and says so.
* **Side-effect ledger.** `ledger.jsonl`, outside every candidate's state store. Each tool
  invocation appends one line `{"tool","args","invocation","pid","t"}` and `fsync`s **before**
  returning its result. The ledger is the ground truth for how many times an effect happened.
* **Tools:**
  * `lookup(key)` is an observation: it returns a fixed value and writes to the ledger.
  * `charge(account, amount)` is a consequential effect: it writes to the ledger. If the candidate
    supports an idempotency key, the harness passes one and records whether the candidate
    forwarded it.

### E1: minimal tool use

Goal: "Charge account `acct-7` the amount `lookup('price')` returns, then report the charge."

The scripted model replies:
1. call `lookup(key="price")`;
2. call `charge(account="acct-7", amount=<value>)`;
3. a final answer naming the amount.

The outcome is **verified** only when the ledger holds exactly one `lookup` and exactly one
`charge` with the right amount. The model's final answer is not the outcome. Record: success,
model calls, tool calls, invalid tool arguments (from one extra run whose scripted reply omits a
required argument), retries, tokens (if the fake model reports any), and wall time.

### E2: crash recovery

The E1 task runs with the candidate's durable persistence enabled (its most durable built-in
option). Experiments must survive a real process death, so an in-memory store counts only for D1
analysis, never for D2 to D4 results. The process is killed with `SIGKILL` (never a graceful
signal) at each window:

| Window | Kill point |
| --- | --- |
| W1 | before the `charge` tool is invoked (after the model chose it) |
| W2 | inside `charge`, after the ledger line is fsynced, before the tool returns |
| W3 | after `charge` returned and the candidate recorded its result |
| W4 | during the candidate's own state write (between "about to persist" and "persisted"), where a hook exists |
| W5 | during a handoff or transfer between agents, where the candidate has handoffs |
| W6 | while suspended awaiting human approval, where the candidate has approval |

Kill points are reached deterministically. The harness signals at an exact point, for example a
tool that kills its own process after the ledger write for W2. It never uses timing.

A fresh process then resumes using the candidate's documented resume API. For each window, record:
* `resumed`: whether resumption worked without manual reconstruction;
* `charges_in_ledger`: the number of charge lines;
* `final_state_consistent`: whether the candidate's state agrees with the ledger;
* `progress_lost`: whether a step recorded before the kill had to be redone;
* `manual_intervention`: what, if anything, a person had to do;
* `outcome`: one of `correct` (one charge, task finished), `duplicate_effect`, `lost_progress`,
  `stuck`, `crashed_on_resume` or `not_applicable` (the window has no meaning for this candidate).

### E3: handoff and approval

Triage → Specialist → human approval → resume → verify. Triage hands control to Specialist.
Specialist proposes `charge`, which needs approval. The run suspends, a new process approves, and
the run resumes and charges. Record:
* the context passed at each boundary (full history, a filtered history, a summary, or only tool
  results);
* who owns state at each step;
* the execution identifiers;
* what happens if Specialist fails;
* whether approval survives a process restart, which is E2's W6.

### E4: small-model effectiveness

The same local model, tasks, context budget and inference settings across candidates. **Blocked in
this environment**: no local inference endpoint is reachable (`localhost:1235` refuses connections,
and no local-model variables are set). No substitute figures are used.

### E5: runtime overhead

The E1 task with the fake model and fake tools, so the measurement isolates orchestration:
serialization, scheduling, persistence and dispatch. Run 1 warm-up plus N = 50 repetitions,
in-memory and durable store separately where both exist, and report the median, p10 and p90 wall
time per run, plus import or cold-start time measured separately. The environment line in
`results.json` records the CPU count, Python or Rust version, package versions and the machine.
Only numbers from this harness on this machine are compared.

## results.json

```json
{
  "candidate": "langgraph",
  "commit": "<sha>",
  "package": "langgraph==x.y.z",
  "environment": {"cpus": 4, "python": "3.13.x", "os": "linux"},
  "e1": {"success": true, "model_calls": 3, "tool_calls": 2, "invalid_args_handled": "...", "wall_ms": 0},
  "e2": {"store": "sqlite", "windows": {"W1": {"resumed": true, "charges_in_ledger": 1, "outcome": "correct", "notes": "..."}}},
  "e3": {"supported": true, "notes": "..."},
  "e5": {"repetitions": 50, "in_memory_ms": {"p10": 0, "median": 0, "p90": 0}, "durable_ms": {}},
  "blocked": ["..."]
}
```

A field the harness did not measure is absent or `null`, never `0`.
