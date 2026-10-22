# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792700911`.

## Trials

| Task | Provider | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| E | local-object | 1 | yes | 3/6 | no | no | no | no | no | yes | invalid_decision | 5.6 |
| E | local-object | 2 | yes | 3/6 | no | no | no | no | no | yes | invalid_decision | 4.5 |
| E | local-object | 3 | yes | 3/6 | no | no | no | no | no | yes | invalid_decision | 4.4 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| E | local-object | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 4.5 / 4.4 / 5.6 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
