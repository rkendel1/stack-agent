# P3: durable operator settlement and lease release, measured

What P3 changes is described in
[`docs/product/agent-state.md`](../../../../../product/agent-state.md#operator-settlement):

* **The settle command.** `stack-agent agent-state settle` records an operator's conclusion about an
  intent whose outcome was never observed: who decided, what they concluded, and on what evidence.
  It is recorded as a conclusion, never as an observation or a receipt.
* **Lease release.** A resume refused with `OutcomeUncertain` gives back the lease it took.
* **Resuming from a settlement.** A resumed run restores the recorded trajectory. It does not
  perform again an action settled as executed, and may perform one settled as not executed.

Two measurements, reported separately:

1. **The crash harness** (`../harness.py`): real `SIGKILL`s, real release binaries, a ledger oracle.
   It is the same harness and fake model as before, with two changes:
   * the operator step uses the binary's own command where it has one (`sa-state reconcile`
     otherwise);
   * one window is added, W2-refusal-default-lease.
2. **The durability bench** (`stack-agent --durability-bench`): 20 repeats per cell, deterministic.

The binaries:

* **Before (P2):** the release build of `main` at `2eadaa4` (#18 merged). It settles through
  `sa-state reconcile`, built against P2's `chip-agent-state`.
* **After (P3):** the release build of this branch at `c4e0a80`.

In every number below, **the ledger (or, in the bench, the world on disk) is the measure of
effects.** A successful settlement is never counted as evidence that the effect occurred.

## 1. Crash harness

The operator is the harness. It reads the ledger written by the killed process and concludes
`executed` if the ledger has a line, `not_executed` otherwise. It records that through the shipped
command (after) or `sa-state reconcile` (before), then resumes.

### E2: windows that need an operator

| Window | Operator's conclusion | Before (P2): charges, final status | After (P3): charges, final status | Operator steps | Refusal to re-acquire, before → after |
| --- | --- | --- | --- | --- | --- |
| W2 | executed | **2** (duplicate), completed | **1**, blocked | 1 → 1 | 1508 ms → **17 ms** |
| W4c | executed | **2** (duplicate), completed | **1**, blocked | 1 → 1 | 30511 ms → **17 ms** |
| W4-torn-obs | executed | **2** (duplicate), completed | **1**, blocked | 1 → 1 | 30514 ms → **22 ms** |
| W1b | not executed | 1, completed | 1, completed | 1 → 1 | 1508 ms → **13 ms** |
| W4d | not executed | 1, completed | 1, completed | 1 → 1 | 30507 ms → **15 ms** |

**The re-acquire time.**

* *Before:* the time from the refused resume's exit to the next resume is the refusing run's lease,
  waited out, plus 0.5 s of margin. That is 1 s in these windows, and 30 s in the gdb-driven ones.
* *After:* it is the settle command itself. The refusal had already given its lease back.

**After a settlement of `executed`** (W2, W4c and W4-torn-obs):

* The resumed process restores the recorded `project.read` (`progress_lost: false`, 1 model call)
  and does not run `pax.test` again. The ledger keeps its one line, from the killed invocation.
* The fake model still asks for `pax.test`. It was not changed and does not read the "Ruled out"
  line. The durable executor refuses before recording an intent.
* The work ends `blocked` (exit 1) with this reason: "pax.test was settled by harness-operator as
  having taken effect (intent a2.e2-task-exec-2); its outcome was never observed, so it is not
  performed again, and nothing was executed".
* The harness labels this `stopped_after_settlement`. The verify goal needs the test's result, which
  was never observed. Completing on the operator's word would fabricate it, and running the test
  again would repeat the charge. So the work stops, no duplicate is created and no receipt is made
  up. The report's `executions_by_capability` counts the refused request (`pax.test: 1`); the ledger
  shows nothing ran.

**After a settlement of `not_executed`** (W1b and W4d): the resumed process performs `pax.test`
through the normal path, which is the ledger's one line, and completes. Nothing required is skipped.

Before P3, W2, W4c and W4-torn-obs charged twice. The operator's record carried no request, so the
next attempt restarted from the goal and charged again.

### The new window: W2-refusal-default-lease

W2, with the killed owner's 1 s lease waited out. The refused resume and the next resume both use
the default lease (900 s).

| | Before (P2) | After (P3) |
| --- | --- | --- |
| first resume | exit 3, `OutcomeUncertain` | exit 3, `OutcomeUncertain` |
| operator settles | exit 0 (sa-state) | exit 0 (`agent-state settle`) |
| next resume, at once | **exit 3, lease held** (by the refusal, for 900 s) | **runs** 121 ms later, exit 1 (blocked, as W2) |
| charges | 1 | 1 |

### Unchanged

* **Windows that need no operator are identical:** W1a, W1a-model, W3, W4a, W4b, W4-torn-outcome and
  W4-torn-final end correct with 1 charge, before and after.
* **W2-default-lease is unchanged (exit 3):** the dead owner holds the task for 900 s, and contract v1
  has no forced takeover. P3 releases only a lease held by a refusal.
* **E1:** both variants are as before. That is 1 charge, 2 model calls and 2 executions, and both
  invalid-argument runs exit 1 with 0 ledger lines.
* **The records changed.** The intents' `action` now carries the request's binding and inputs
  (`pax.test binding=b1.… inputs={}`), which is how the settlement names exactly the `pax.test` it
  settles.

### E5: overhead (N=50 after 1 warm-up, whole process, alternating binaries)

| Round | Binary | in-memory p10 / median / p90 (ms) | durable p10 / median / p90 (ms) |
| --- | --- | --- | --- |
| 1 | main `2eadaa4` | 88.52 / 104.62 / 121.25 | 107.56 / 115.59 / 138.60 |
| 1 | P3 | 86.83 / 102.14 / 123.56 | 108.40 / 124.83 / 153.97 |
| 2 | main `2eadaa4` | 86.64 / 100.31 / 115.58 | 105.77 / 116.71 / 139.38 |
| 2 | P3 | 89.30 / 106.75 / 121.13 | 105.94 / 116.01 / 146.08 |
| 3 | main `2eadaa4` | 91.51 / 102.09 / 119.81 | 105.14 / 122.10 / 140.73 |
| 3 | P3 | 86.42 / 96.54 / 118.79 | 111.75 / 118.33 / 138.29 |

The durable median for P3 against main moved by +8.0%, −0.6% and −3.1%. That is noise in both
directions, and within the 10% budget. The uninterrupted path adds one read of each intent's
observation at `begin`, to find settlements. All runs verified. The raw records are in `e5/`.

## 2. Durability bench

The operator step is `chip_agent_state::settle`, the function the command calls, with the
conclusion read from the world on disk. A refusal no longer costs a lease wait: a released lease
ends 1 ms later, and the bench models the refusal's and the settlement's at 1 ms each.

**Every cell of arm A, and every arm-B cell of the contention, crash-between-actions, normal and
deduplicated rows, is identical before and after.** The arm-B cells that changed:

| scenario | task | verified | false success | duplicates | refusals | operator | model calls | executions | lease wait (modelled) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| crash after effect | notify | 0/20 → **20/20** | 20 → **0** | 20 → **0** | 20 → 20 | 20 → 20 | 8 → 5 | 7 → 4 | 120 002 → **60 003** ms |
| crash after effect | steps | 20/20 | 0 | 0 | 20 → 20 | 20 → 20 | 8 → 7 | 7 → 6 | 120 002 → **60 003** ms |
| crash before effect | notify | 20/20 | 0 | 0 | 20 → 20 | 20 → 20 | 7 | 6 | 120 002 → **60 003** ms |
| crash before effect | steps | 20/20 | 0 | 0 | 20 → 20 | 20 → 20 | 9 | 8 | 120 002 → **60 003** ms |

* **Crash after effect, notify.** The case P2 left open is now verified 20/20 with one notification.
  The bench's scripted model reads the "Ruled out" line Chip sends ("capability world.notify
  (step=1): an operator (bench-operator) concluded that its effect occurred …") and does not notify
  again. The world on disk holds exactly one notification. The settlement itself is not counted as
  evidence: the verdict comes from the world.
* **Fewer model calls and executions, and why.** They come from restoring the trajectory past the
  settlement instead of restarting from the goal. They are not evidence of anything about the effect.
* **Lease wait.** The remaining 60 001 ms is the crashed owner's lease, which still has to be waited
  out. The refusal's own lease is no longer waited for.
* **Stale owner and fencing are unchanged.** Stale-owner rows are identical. The fenced worker's
  commits are still refused (`FENCE_STALE`).

## Review gate: the full sequence (W2, after)

From `after-p3/logs/e2_raw.json`, window W2:

1. **Crash during the effect.** The fake `pax.test` fsyncs its ledger line and `SIGKILL`s the agent.
   The ledger has 1 line; the `pax.test` intent is `intended` with no observation.
2. **`OutcomeUncertain`.** A fresh process exits 3 ("has actions whose outcome is unknown
   (a2.e2-task-exec-2)"). It took the task at fence 2 and gave its lease back.
3. **Operator settles with identity and evidence.** This took 17 ms after the refusal, with no lease
   wait:

   ```text
   stack-agent agent-state settle --task-id e2-task --intent a2.e2-task-exec-2 --executed true \
       --decided-by harness-operator --evidence "harness ledger has 1 line(s) at the kill"
   ```

   It exits 0. The intent is resolved with conclusion `executed`, citing the settlement record. A
   checkpoint by `harness-operator` states the conclusion and the evidence.
4. **A fresh process resumes from FeltDB.** It is attempt 4 at fence 4. It restores the
   `project.read` observation and makes 1 model call.
5. **Correct continuation, nothing fabricated.**
   * `pax.test` is not performed again: the ledger still has 1 line.
   * The work stops `blocked`, naming the settlement.
   * No receipt or observation was made for the settled charge.
   * The task is not `completed`: nothing observed the test's result.

## Reproduce

```sh
R=/home/user/stack-agent; X=$R/docs/research/agent-runtime-primitives/experiments/stack-agent
W=/tmp/p3   # any scratch directory
cd $R && CARGO_TARGET_DIR=$W/target cargo build --locked --release --bin stack-agent
(cd $X/sa-state && CARGO_TARGET_DIR=$W/target cargo build --offline --release)   # the sa-state lockfile is not committed
mkdir -p $W/bin && cp $W/target/release/{stack-agent,sa-state} $W/bin/
export PATH=/path/to/pax-v0.4.1/target/release:$PATH          # pax 674f3b3, as in CI
python3 $X/harness.py $W/bin $W/work $W/out e1
python3 $X/harness.py $W/bin $W/work $W/out e2               # needs gdb with ptrace allowed
python3 $X/harness.py $W/bin $W/work $W/out e5 --n 50        # alternate with the baseline's bin directory
$W/bin/stack-agent --durability-bench --repeats 20 --out $W/bench
```

The baseline uses the same commands, with `main`'s binary and an `sa-state` built from `main`.
`assemble.py` writes the provenance fields of `before-p2/results.json` and `after-p3/results.json`;
`commit`, `commit_note` and `package` were corrected afterwards to name the builds.
