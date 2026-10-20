# P2: stable idempotency keys, measured

What P2 changes is described in [`docs/product/agent-state.md`](../../../../../product/agent-state.md#idempotency-keys).
In short, every logical action of a durable task gets a key that is the same in every attempt and
process. A capability that carries keys receives it. After a crash, a capability that declares it
deduplicates may be asked again under that key; every other capability's unknown outcome is still
refused.

Two measurements follow, reported separately. They answer different questions:

1. **The crash harness** (`../harness.py`, real `SIGKILL`s, the real release binary, a ledger
   oracle). It is unchanged from P1. Its charge is `pax.test`, which runs a fake `pax` through a
   fixed argv. That capability cannot carry a key, so the harness measures what P2 must *not* change.
2. **The durability bench** (`stack-agent --durability-bench`, 20 repeats per cell). It adds one
   task, `notify-once-deduplicated`, whose notification service is a fake that applies one
   notification per key and declares it. The bench measures what P2 does when a capability can
   deduplicate.

The binaries:

* "Before" is the P1 release binary at `65f34c6`, which is `main` after #15. It is the same binary
  as `../p1-trajectory-resume`.
* "After" is the release binary built from `p2/stable-idempotency` (`09bd163`; the bench was re-run after its task set was renamed `v2`, with identical counts).

## 1. Crash harness (E1, E2, E5): no capability here deduplicates

### E2: crash windows

| Window | Before (P1): outcome, charges | After (P2): outcome, charges |
| --- | --- | --- |
| W1a, W1a-model, W3, W4a, W4b, W4-torn-outcome, W4-torn-final | correct, 1 | correct, 1 |
| W1b, W4d | stuck → correct, 1 | stuck → correct, 1 |
| W2 | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 |
| W4c | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 |
| W4-torn-obs | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 |
| W2, default 900 s lease | stuck (exit 3, lease held), 1 | stuck (exit 3, lease held), 1 |

Every window is identical.

* **W2, W4c and W4-torn-obs.** The process died after `pax.test` charged and before its observation
  was recorded. The resume is still refused with `OutcomeUncertain` (exit 3). After the operator
  settles the intent from the ledger, the next run still charges again. P2 does not and must not
  change this:
  * `pax.test` is `not_carried`, so no key reaches it;
  * nothing could deduplicate the second charge;
  * resuming without the refusal would be a guessed retry.
* **The records are unchanged.** In `logs/e2_raw.json`, the intents' `action` is still the bare
  capability id (`project.read`, `pax.test`), byte for byte what P1 wrote. An intent records a key
  only for a capability that carries one.

The settled-then-repeated duplicate is the gap P3 (an operator settlement that the next attempt can
restore) addresses. It is not claimed fixed here.

### E1: unchanged

1 ledger line, task `completed`, 2 model calls, 2 executions. Both invalid-argument variants exit 1
with 0 ledger lines.

### E5: overhead (N=50 after 1 warm-up, whole process, alternating binaries)

| Round | Binary | in-memory p10 / median / p90 (ms) | durable p10 / median / p90 (ms) |
| --- | --- | --- | --- |
| 1 | P1 `65f34c6` | 81.29 / 90.06 / 102.77 | 102.47 / 115.76 / 133.08 |
| 1 | P2 `09bd163` | 80.63 / 88.01 / 103.43 | 96.08 / 103.80 / 119.78 |
| 2 | P1 `65f34c6` | 84.18 / 90.93 / 107.42 | 102.87 / 110.77 / 131.62 |
| 2 | P2 `09bd163` | 85.60 / 93.88 / 105.79 | 97.75 / 105.40 / 123.62 |
| 3 | P1 `65f34c6` | 84.42 / 91.41 / 109.08 | 100.83 / 108.66 / 121.02 |
| 3 | P2 `09bd163` | 87.03 / 95.56 / 108.74 | 99.71 / 114.31 / 137.75 |

The durable median for P2 against P1 moved by −10.3%, −4.8% and +5.2%. This is run-to-run noise on a
shared machine in both directions. No overhead is measurable. P2 adds two things to the durable
path:

* one read of the task's intents at `begin`, which was already done;
* a map lookup per execution.

All 50 runs of every round were verified. The raw records are `logs/e5_p1_round*.json` and
`logs/e5_p2_round*.json`.

## 2. Durability bench: a capability that deduplicates

Arm A runs Chip's loop without durable state. Arm B runs it as `work --state-db` composes it. Both
are deterministic: 20 repeats per cell, every repeat identical. Summaries:

* before: `bench/before-p1/summary.{json,md}`;
* after: `bench/after-p2/summary.{json,md}`.

**Every cell of the two existing tasks (`steps-3-observable`, `notify-once-unobservable`), in both
arms and every scenario, is identical before and after.** The only change in those cells is the two
new columns, which read 0.

The new task, `notify-once-deduplicated`, uses the same world, model and faults as
`notify-once-unobservable`. Its notification service is a fake that keeps each notification's key
and answers a key it has already sent under, instead of sending again. The table puts the new task's
results next to the unobservable task, which is the same service with no key and no declaration:

| scenario | task | arm | verified | false success | refusals | operator | duplicate effects | dedup prevented | reissued intents | model calls | executions | runs | lease wait (modelled) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| crash after effect | unobservable | B | 0/20 | 20 | 20 | 20 | 20 | 0 | 0 | 8 | 7 | 3 | 120 s |
| crash after effect | **deduplicated** | B | **20/20** | **0** | **0** | **0** | **0** | **20** | 20 | 7 | 6 | 2 | 60 s |
| crash after effect | deduplicated | A | 0/20 | 20 | 0 | 0 | 20 | 0 | 0 | 8 | 7 | 2 | 0 |
| crash before effect | unobservable | B | 20/20 | 0 | 20 | 20 | 0 | 0 | 0 | 7 | 6 | 3 | 120 s |
| crash before effect | **deduplicated** | B | 20/20 | 0 | **0** | **0** | 0 | 0 | 20 | 7 | 6 | 2 | 60 s |
| stale owner | unobservable | B | 20/20 | 0 | 20 | 0 | 0 | 0 | 0 | 6 | 5 | 3 | 120 s |
| stale owner | **deduplicated** | B | 20/20 | 0 | **0** | 0 | 0 | **20** | 20 | 7 | 6 | 2 | 60 s |
| crash between actions, concurrent start, normal | deduplicated | B | 20/20 | 0 | 0 | 0 | 0 | 0 | 0 | as unobservable | | | |

What this shows, and what it does not:

* **What prevented the duplicate.** "dedup prevented" is the fake service's own count of requests it
  answered from a notification it had already sent under the same key. It is counted at the effect,
  in `replays.log`, and judged with the world on disk. The outbox holds exactly one notification in
  every one of those 20 cases.
  * Fewer model calls (7 instead of 8) are not the evidence. They come from not restarting from the
    goal after an operator settlement.
* **Crash after the effect.** The resumed attempt was not refused. It restored the three earlier
  observations, the model asked for the notification again, and the request went out under the same
  key. The service answered without sending, and the open intent was reconciled from that answer.
  There was no operator step and one fewer restart.
* **Crash before the effect.** The intent was open with nothing sent. The re-issue sent the one
  notification (0 prevented, 20 reissued), again with no refusal or operator.
* **Stale owner.** The second worker took over and sent under the key. When the first worker woke and
  sent under the same key, the service answered instead of sending again (20 prevented). The stale
  owner's outcome is still refused by the fence, as before.
* **Arm A with the same service still sends twice.** Without durable state, nothing assigns a key,
  so deduplication has nothing to work with.
* **The guarantee is the service's.** These are a fake service's numbers. A real capability earns
  the `deduplicated` declaration only if its effect owner provides that guarantee. Today no
  production capability does (see the classification in `agent-state.md`).

## Reproduce

```sh
R=/home/user/stack-agent; X=$R/docs/research/agent-runtime-primitives/experiments/stack-agent
W=/tmp/p2   # any scratch directory
cd $R && CARGO_TARGET_DIR=$W/target cargo build --locked --release --bin stack-agent
(cd $X/sa-state && CARGO_TARGET_DIR=$W/target cargo build --offline --release)   # the sa-state lockfile is not committed
mkdir -p $W/bin && cp $W/target/release/{stack-agent,sa-state} $W/bin/
export PATH=/path/to/pax-v0.4.1/target/release:$PATH          # pax 674f3b3, as in CI
python3 $X/harness.py $W/bin $W/work $W/out e1
python3 $X/harness.py $W/bin $W/work $W/out e2               # needs gdb with ptrace allowed
python3 $X/harness.py $W/bin $W/work $W/out e5 --n 50        # alternate with the P1 binary's bin directory
$W/bin/stack-agent --durability-bench --repeats 20 --out $W/bench-after
```

`assemble.py` writes the baseline's provenance fields. In this directory, `commit`, `commit_note` and
`package` were corrected afterwards to name the P2 build.
