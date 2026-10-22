# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792700350`.

## Trials

| Task | Provider | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| E | local-none | 1 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 61.7 |
| E | local-none | 2 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 27.9 |
| E | local-none | 3 | yes | 7/7 | yes | no | yes | yes | yes | yes | none | 13.4 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| E | local-none | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 27.9 / 13.4 / 61.7 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
