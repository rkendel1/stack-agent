# stack-agent: E1, E2, E5 against the real binary

stack-agent measured with the same harness discipline as the frameworks (protocol.md). It ran the
release binary of origin/main @ `366f6c1` from `/home/user/stack-agent-research`, with a scripted
fake model, a fake `pax` and real `SIGKILL`s. No production code, test or Cargo file was changed.
Results are in `results.json`, and raw per-run records are in `logs/`.

## Fixture mapping

stack-agent's capabilities are fixed, so the shared fixture is mapped onto them like this:

| Protocol | stack-agent |
| --- | --- |
| `lookup(key)` | `project.read src/lib.rs` (Chip's own executor; not ledgered) |
| `charge(...)` | `pax.test`: `fake-pax/pax` is first on `PATH`. It appends one line to `ledger.jsonl` (outside the project and the db) and fsyncs it, then prints a valid `pax.execution-result.v1` (`passed`). Cargo never runs |
| final answer | none. With `--kind verify`, Chip's local policy completes the work itself once `pax.test` passed, so there are 2 model calls, not 3 |
| durable store | `--state-db FILE --task-id ID --lease-secs N` (FeltDB file log) |
| resume API | the same command again in a new process (`begin` takes the task over once the lease has expired) |

Fake model (`fake_model.py`, stdlib only): an OpenAI-compatible SSE `/v1/chat/completions`. It
picks its reply from the request content, not from a counter:

* If the request contains no `project.read` observation, it replies `project.read`.
* Otherwise it replies `pax.test`.

A restarted process therefore gets the right next decision.

`sa-state/` is a small harness binary built on `chip-agent-state`'s public API, outside the
workspace. It does two things:

* `history`: dumps the task history.
* `reconcile`: performs the operator step for `OutcomeUncertain`. It writes the
  `chip.Observation` record exactly as `crates/stack-agent/src/durability_bench.rs`
  (`reconcile_from_world`) does. The product has no command for this step.

## Commit map

One uninterrupted durable run makes these 12 FeltDB commits (`logs/commit_map_raw.json`):

| Commit | Record |
| --- | --- |
| T1 | create_task |
| T2 | checkpoint `running` |
| T3 | renew |
| T4 | record_intent(read) |
| T5 | Observation(read) |
| T6 | record_outcome(read) |
| — | **`pax --version` probe** (chip-pax validates the chosen pax.test) |
| T7 | renew |
| T8 | record_intent(pax.test) |
| — | **pax test runs** |
| T9 | Observation(pax.test) |
| T10 | record_outcome(pax.test) |
| T11 | chip.WorkResult |
| T12 | checkpoint `completed` |

Each commit is `openat(O_APPEND)`, then 390 to 870 one-byte-scale `write()` calls, then
`fdatasync()`. All of them run on the main thread. The many small writes come from
`serde_json::to_writer` on an unbuffered `File` (feltdb `lib.rs:5728`). A `SIGKILL` can
therefore land inside a commit and leave a torn line.

## Kill points (deterministic, never timed)

* **W1a.** The fake pax `SIGKILL`s its parent on the `--version` probe. At that point the model
  has chosen `pax.test` and no intent has been recorded.
* **W1a-model.** The fake model `SIGKILL`s the agent pid while it is being asked for the decision
  that leads to pax.test. The launching shell writes `$$` to a pidfile and then `exec`s, so the
  pid is the agent's.
* **W1b and W2.** The fake pax kills its parent before or after its fsynced ledger line.
* **W3 and W4\*.** `gdb_kill.py` sets ptrace syscall catchpoints. File descriptors are resolved
  through `/proc/<pid>/fd`, so only db writes count. The kill lands at an exact commit boundary
  (`commit:N:before|after`), at the K-th `write()` of commit N (a torn line), or at the entry of
  a commit's `fdatasync`.
* **strace was rejected for this.** Its `inject=...:when=N` counts every call of the syscall,
  ignoring `-P`.

Leases:

* W1 and W2 crash runs use `--lease-secs 1`.
* gdb runs use 30 s, because ptrace slows the agent past a 1 s lease.
* The harness waits lease + 0.5 s before each resume. This wait is for lease expiry; it is never
  a kill trigger.

## Results (outcome per protocol)

On the outcome labels:

* `lost_progress` means one charge and the task completed, but the `project.read` recorded
  before the kill was executed again.
* After the kill, the loop always restarts from the goal with no observations.
* `stuck` means the first resume was refused. The resolution of each refusal is in the last
  column.

| Window | Persisted at kill | First resume | After manual resolution | Ledger |
| --- | --- | --- | --- | --- |
| W1a | T1–T6 | exit 0, attempt 3 re-reads and charges | — | 1, `lost_progress` |
| W1a-model | T1–T6 | same as W1a | — | 1, `lost_progress` |
| W1b | T1–T8, pax.test intent open | exit 3 `OutcomeUncertain`, `stuck` | operator `executed=false`, then rerun: 1 charge, `lost_progress` | 1 |
| W2 | T1–T8, intent open, ledger 1 | exit 3, `stuck` | operator `executed=true`, then rerun charges again: `duplicate_effect` | 2 |
| W3 | T1–T10 (outcome durable) | exit 0, re-runs from goal | — | 2, `duplicate_effect` |
| W4a | T1–T9 (obs, no outcome) | auto-reconciled from obs, re-run | — | 2, `duplicate_effect` |
| W4b | T1–T11 (WorkResult, task `running`) | re-run as attempt 3 | — | 2, `duplicate_effect` |
| W4c | T1–T8, effect done, T9 not started | exit 3, `stuck` | `executed=true`: `duplicate_effect` | 2 |
| W4-torn-obs | T9 torn (1044 B tail) | tail truncated on open; exit 3, `stuck` | `duplicate_effect` | 2 |
| W4-torn-outcome | T10 torn (1495 B) | truncated; reconciled from T9; re-run | — | 2, `duplicate_effect` |
| W4-torn-final | T12 torn (1629 B) | truncated; re-run | — | 2, `duplicate_effect` |
| W4d | T8 written, killed at its fdatasync entry | the T8 line survived (page cache); exit 3, `stuck` | `executed=false`: `lost_progress` | 1 |
| W2, default lease | as W2 | exit 3 "unexpired lease", `stuck` for up to 900 s | — | 1 |
| W5, W6 | — | `not_applicable` (no handoff, no approval) | — | — |

Further observations:

* **A refused resume takes the lease.** A resume refused with `OutcomeUncertain` has already
  acquired the task: the fence goes from 1 to 2 under a new lease (`begin` calls `acquire`
  before reconciling, `chip-agent-state/src/lib.rs:719` vs `:752-795`). After the operator
  writes the observation, a second lease period must pass. With the default lease that is two
  waits of 900 s.
* **No idempotency key reaches the effect.** The fake pax only ever saw the argv
  `--dir <root> --json test`.
* **E1:** verified. Ledger 1, task `completed`, 2 model calls, 2 executions.
* **E1 invalid arguments** (`project.read` with no `path`, or `pax.test` with an extra input):
  * Chip rejects the input during validation, before any intent is recorded.
  * No retry is asked of the model. The work ends `blocked` (exit 1) and the ledger stays at 0.
* **E5** (N=50 after 1 warm-up, fresh project per run, wall time of the whole process):
  * legacy: p10 78.9, median 83.3, p90 102.8 ms;
  * durable: p10 95.2, median 101.7, p90 115.2 ms;
  * cold start (usage error): median 3.3 ms.

## Reproduce

```sh
R=/home/user/stack-agent-research; X=$R/docs/research/agent-runtime-primitives/experiments/stack-agent
W=/tmp/claude-0/-home-user/f0b9b00d-6e48-5867-8ab3-f70b50f6c1cc/scratchpad/research/stack-agent-e2
cd $R && CARGO_TARGET_DIR=$W/target cargo build --locked --release --bin stack-agent   # feltdb from the local cargo git cache
mkdir -p $W/sa-state && cp -r $X/sa-state/* $W/sa-state/ && cp $R/Cargo.lock $W/sa-state/
(cd $W/sa-state && CARGO_TARGET_DIR=$W/target cargo build --offline --release)
mkdir -p $W/bin && cp $W/target/release/{stack-agent,sa-state} $W/bin/
python3 $X/harness.py $W/bin $W/work $W/out all --n 50     # needs gdb (ptrace allowed) and python3
cp $W/out/logs/*.json $X/logs/ && python3 $X/assemble.py $X
```

The full run takes about 12 minutes. Most of that time is the E2 lease waits and the gdb-traced
runs.
