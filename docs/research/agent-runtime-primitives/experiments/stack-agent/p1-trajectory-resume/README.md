# P1: resume the recorded trajectory from FeltDB, measured

These are the same harness (`../harness.py`), fake model, fake `pax`, `gdb` kill points and ledger
oracle as `../README.md`. They ran against the release binary built from the P1 branch at `65f34c6`
(on `main` `90df4f7`). Later commits on the branch change only documentation. Nothing in the harness was changed. "Before" is `../results.json` (`366f6c1`),
whose agent-state path is unchanged on `main`. E5 was re-measured for both binaries in the same
session.

## E2: crash windows (real `SIGKILL`, fresh-process resume, ledger ground truth)

| Window | Before: outcome, charges | After: outcome, charges | `progress_lost` before → after |
| --- | --- | --- | --- |
| W1a | lost_progress, 1 | **correct, 1** | true → **false** |
| W1a-model | lost_progress, 1 | **correct, 1** | true → **false** |
| W1b | stuck → lost_progress, 1 | stuck → **correct, 1** | true → **false** |
| W2 | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 | true → true |
| W3 | duplicate_effect, 2 | **correct, 1** | true → **false** |
| W4a | duplicate_effect, 2 | **correct, 1** | true → **false** |
| W4b | duplicate_effect, 2 | **correct, 1** | true → **false** |
| W4c | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 | true → true |
| W4-torn-obs | stuck → duplicate_effect, 2 | stuck → duplicate_effect, 2 | true → true |
| W4-torn-outcome | duplicate_effect, 2 | **correct, 1** | true → **false** |
| W4-torn-final | duplicate_effect, 2 | **correct, 1** | true → **false** |
| W4d | stuck → lost_progress, 1 | stuck → **correct, 1** | true → **false** |
| W2, default 900 s lease | stuck (exit 3, lease held), 1 | stuck (exit 3, lease held), 1 | — |
| W5, W6 | not applicable | not applicable | — |

"stuck → X" means the first resume refused (exit 3). The operator then settled the open intent from
the ledger (`sa-state reconcile`), and the next resume ended as X.

What changed and what did not:

* **Every P1 window now ends with exactly one charge and no lost progress.** That covers W1a, W3,
  W4a, W4b, the torn outcome and the torn final record. W1b and W4d, which need the operator first,
  also recover without redoing the read.
* **W2 still refuses to resume with `OutcomeUncertain` (exit 3).** So do W4c and W4-torn-obs, the
  other windows where the effect may have happened with no observation recorded. P1 does not change
  this refusal.
* **After the operator settles one of those windows, the next run still charges again.** The
  operator's record carries no request, so the history cannot be restored whole, and the attempt
  starts from the goal as before. This is the gap P2 (an idempotency key the executor can
  de-duplicate on) and P3 (operator settlement) address. It is not claimed fixed here.

## E1: unchanged

1 ledger line, task `completed`, 2 model calls, 2 executions. Both invalid-argument variants
(`project.read` with no `path`, `pax.test` with an extra input) exit 1 with 0 ledger lines, as before.

## E5: overhead (N=50 after 1 warm-up, whole process, alternating binaries)

| Round | Binary | in-memory p10 / median / p90 (ms) | durable p10 / median / p90 (ms) |
| --- | --- | --- | --- |
| 1 | main `90df4f7` | 83.65 / 91.12 / 108.99 | 98.96 / 114.05 / 136.21 |
| 1 | P1 | 81.61 / 90.50 / 107.35 | 98.92 / 109.50 / 126.74 |
| 2 | main `90df4f7` | 82.17 / 89.20 / 111.51 | 97.40 / 114.78 / 138.81 |
| 2 | P1 | 82.93 / 93.35 / 106.03 | 100.52 / 116.82 / 138.48 |
| 3 | main `90df4f7` | — / 99.43 / — | 99.46 / 114.97 / 133.15 |
| 3 | P1 `65f34c6` | — / 92.97 / — | 98.02 / 109.83 / 127.79 |
| 4 | main `90df4f7` | — / 96.74 / — | 103.00 / 112.78 / 134.00 |
| 4 | P1 `65f34c6` | — / 95.21 / — | 101.37 / 111.56 / 126.37 |

(Rounds 1 and 2 used a P1 build from just before restore was made all-or-nothing, a change that
only affects a history that cannot be restored, which an uninterrupted run never has. Rounds 3 and 4
are the final build. The raw records are `logs/e5_main_round*.json` and `logs/e5_p1_round*.json`.)

Durable median, P1 against main: −4.0%, +1.8%, −4.5% and −1.1% over the four rounds. That is within
run-to-run noise and inside the 10% budget.

**Limitation:** E5 measures an uninterrupted run, which restores nothing. It does not isolate the
cost of reading a trajectory back on a resume. Each observation record is also larger now, because
it carries the validated request.

## Reproduce

```sh
R=/home/user/stack-agent; X=$R/docs/research/agent-runtime-primitives/experiments/stack-agent
W=/tmp/p1   # any scratch directory
cd $R && CARGO_TARGET_DIR=$W/target cargo build --locked --release --bin stack-agent
(cd $X/sa-state && CARGO_TARGET_DIR=$W/target cargo build --offline --release)   # the sa-state lockfile is not committed
mkdir -p $W/bin && cp $W/target/release/{stack-agent,sa-state} $W/bin/
export PATH=/path/to/pax-v0.4.1/target/release:$PATH          # pax 674f3b3, as in CI
python3 $X/harness.py $W/bin $W/work $W/out e1
python3 $X/harness.py $W/bin $W/work $W/out e2               # needs gdb with ptrace allowed
python3 $X/harness.py $W/bin $W/work $W/out e5 --n 50
cp $W/out/logs/*.json $X/p1-trajectory-resume/logs/ && python3 $X/assemble.py $X/p1-trajectory-resume
```

`assemble.py` writes the baseline's provenance fields (`commit`, `commit_note`). In this directory
they were corrected afterwards to name the P1 build.
