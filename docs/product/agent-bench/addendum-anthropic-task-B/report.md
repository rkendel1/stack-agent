# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792432978`.

## Trials

| Task | Provider | Trial | Provider completed | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| B | anthropic | 1 | yes | no | yes | yes | no | no | yes | invalid_decision | 28.5 |
| B | anthropic | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 25.5 |
| B | anthropic | 3 | yes | no | yes | yes | no | no | yes | invalid_decision | 23.4 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| B | anthropic | 3 | 0 | 0/3 | 0/3 | 2/3 | 0 | 0 | 0 | 3 | 25.5 / 23.4 / 28.5 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
