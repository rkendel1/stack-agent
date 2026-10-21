# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792601510`.

## Trials

| Task | Provider | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local-none | 1 | no | 0/0 | no | no | no | no | no | yes | transport | 0.1 |
| A | local-none | 2 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| A | local-none | 3 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| B | local-none | 1 | no | 0/0 | no | no | no | no | no | yes | transport | 0.1 |
| B | local-none | 2 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| B | local-none | 3 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| C | local-none | 1 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| C | local-none | 2 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| C | local-none | 3 | no | 0/0 | no | no | no | no | no | yes | transport | 0.1 |
| D | local-none | 1 | no | 0/0 | no | yes | n/a | n/a | yes | yes | transport | 0.0 |
| D | local-none | 2 | no | 0/0 | no | yes | n/a | n/a | yes | yes | transport | 0.0 |
| D | local-none | 3 | no | 0/0 | no | yes | n/a | n/a | yes | yes | transport | 0.0 |
| E | local-none | 1 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| E | local-none | 2 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |
| E | local-none | 3 | no | 0/0 | no | no | no | no | no | yes | transport | 0.0 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local-none | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.0 / 0.0 / 0.1 |
| B | local-none | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.0 / 0.0 / 0.1 |
| C | local-none | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.0 / 0.0 / 0.1 |
| D | local-none | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 / 0.0 |
| E | local-none | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.0 / 0.0 / 0.0 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
