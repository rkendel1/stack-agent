# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792452743`.

## Trials

| Task | Provider | Arm | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | legacy | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.2 |
| A | anthropic | durable | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.6 |
| A | anthropic | legacy | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.0 |
| A | anthropic | durable | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.2 |
| A | anthropic | legacy | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 5.9 |
| A | anthropic | durable | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.0 |
| B | anthropic | legacy | 1 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 21.9 |
| B | anthropic | durable | 1 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 21.2 |
| B | anthropic | legacy | 2 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 23.8 |
| B | anthropic | durable | 2 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 23.3 |
| B | anthropic | legacy | 3 | yes | 7/7 | yes | yes | no | yes | no | yes | none | 18.9 |
| B | anthropic | durable | 3 | yes | 7/7 | yes | yes | no | yes | no | yes | none | 17.9 |
| C | anthropic | legacy | 1 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 53.1 |
| C | anthropic | durable | 1 | yes | 30/30 | no | yes | yes | no | no | yes | completion_refused | 54.7 |
| C | anthropic | legacy | 2 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 53.2 |
| C | anthropic | durable | 2 | yes | 30/30 | no | yes | yes | no | no | yes | completion_refused | 51.3 |
| C | anthropic | legacy | 3 | yes | 11/11 | no | yes | no | no | no | yes | invalid_decision | 15.9 |
| C | anthropic | durable | 3 | yes | 12/12 | no | yes | no | no | no | yes | invalid_decision | 18.4 |
| D | anthropic | legacy | 1 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 6.4 |
| D | anthropic | durable | 1 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | escalated | 6.0 |
| D | anthropic | legacy | 2 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 7.3 |
| D | anthropic | durable | 2 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | escalated | 3.7 |
| D | anthropic | legacy | 3 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 7.3 |
| D | anthropic | durable | 3 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | blocked | 3.3 |
| E | anthropic | legacy | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 10.1 |
| E | anthropic | durable | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.7 |
| E | anthropic | legacy | 2 | yes | 6/6 | yes | yes | yes | yes | yes | yes | none | 10.3 |
| E | anthropic | durable | 2 | yes | 6/6 | yes | yes | yes | yes | yes | yes | none | 10.6 |
| E | anthropic | legacy | 3 | yes | 1/2 | no | no | no | no | no | yes | invalid_decision | 10.0 |
| E | anthropic | durable | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 8.7 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Arm | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | durable | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 6.2 / 6.0 / 6.6 |
| A | anthropic | legacy | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 6.0 / 5.9 / 6.2 |
| B | anthropic | durable | 3 | 0 | 2/3 | 3/3 | 2/3 | 0 | 0 | 0 | 0 | 21.2 / 17.9 / 23.3 |
| B | anthropic | legacy | 3 | 0 | 2/3 | 3/3 | 2/3 | 0 | 0 | 0 | 0 | 21.9 / 18.9 / 23.8 |
| C | anthropic | durable | 3 | 0 | 0/3 | 0/3 | 2/3 | 0 | 0 | 0 | 3 | 51.3 / 18.4 / 54.7 |
| C | anthropic | legacy | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 53.1 / 15.9 / 53.2 |
| D | anthropic | durable | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 3.7 / 3.3 / 6.0 |
| D | anthropic | legacy | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 7.3 / 6.4 / 7.3 |
| E | anthropic | durable | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 8.7 / 7.7 / 10.6 |
| E | anthropic | legacy | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 10.1 / 10.0 / 10.3 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.

## Paired arms (durable − legacy)

| task | provider | pairs | both complete | legacy only | durable only | neither | durable recorded | durable false success | median latency diff ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A | anthropic | 3 | 3 | 0 | 0 | 0 | 3 | 0 | 122 |
| B | anthropic | 3 | 2 | 0 | 0 | 1 | 3 | 0 | -687 |
| C | anthropic | 3 | 0 | 2 | 0 | 1 | 3 | 0 | 1561 |
| D | anthropic | 3 | 3 | 0 | 0 | 0 | 3 | 0 | -3574 |
| E | anthropic | 3 | 2 | 0 | 1 | 0 | 3 | 0 | -1325 |
