# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792435729`.

## Trials

| Task | Provider | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 4.9 |
| A | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 3.0 |
| A | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 4.6 |
| A | openai | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 9.1 |
| A | openai | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 11.8 |
| A | openai | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 13.6 |
| A | anthropic | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.2 |
| A | anthropic | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.5 |
| A | anthropic | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.3 |
| A | local-object | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.0 |
| A | local-object | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 3.7 |
| A | local-object | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 3.7 |
| B | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.6 |
| B | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.5 |
| B | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.5 |
| B | openai | 1 | yes | 7/7 | yes | yes | yes | yes | yes | yes | none | 21.9 |
| B | openai | 2 | yes | 7/7 | yes | yes | yes | yes | yes | yes | none | 21.5 |
| B | openai | 3 | yes | 7/7 | yes | yes | yes | yes | yes | yes | none | 20.4 |
| B | anthropic | 1 | yes | 6/6 | yes | yes | no | no | no | yes | none | 22.2 |
| B | anthropic | 2 | yes | 8/8 | yes | yes | yes | no | no | yes | none | 30.4 |
| B | anthropic | 3 | yes | 8/8 | yes | yes | yes | no | no | yes | none | 24.7 |
| B | local-object | 1 | no | 6/6 | no | yes | no | no | no | yes | length_limited | 127.6 |
| B | local-object | 2 | yes | 5/5 | no | yes | no | no | no | yes | invalid_decision | 15.7 |
| B | local-object | 3 | yes | 5/5 | no | yes | no | no | no | yes | invalid_decision | 19.1 |
| C | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 4.8 |
| C | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 0.5 |
| C | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 0.4 |
| C | openai | 1 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 63.8 |
| C | openai | 2 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 73.8 |
| C | openai | 3 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 60.3 |
| C | anthropic | 1 | yes | 14/14 | no | yes | no | no | no | yes | invalid_decision | 25.0 |
| C | anthropic | 2 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 64.9 |
| C | anthropic | 3 | yes | 9/9 | no | yes | no | no | no | yes | invalid_decision | 14.5 |
| C | local-object | 1 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 175.3 |
| C | local-object | 2 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 80.1 |
| C | local-object | 3 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 40.9 |
| D | local | 1 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 4.5 |
| D | local | 2 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 1.0 |
| D | local | 3 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 1.8 |
| D | openai | 1 | yes | 3/3 | yes | yes | n/a | n/a | yes | yes | none | 23.1 |
| D | openai | 2 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 14.7 |
| D | openai | 3 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 14.4 |
| D | anthropic | 1 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | none | 7.7 |
| D | anthropic | 2 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | blocked | 3.3 |
| D | anthropic | 3 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 6.6 |
| D | local-object | 1 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 9.6 |
| D | local-object | 2 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.2 |
| D | local-object | 3 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.1 |
| E | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.4 |
| E | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.4 |
| E | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.3 |
| E | openai | 1 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 10.0 |
| E | openai | 2 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 10.0 |
| E | openai | 3 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 7.8 |
| E | anthropic | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 10.8 |
| E | anthropic | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 9.0 |
| E | anthropic | 3 | yes | 1/2 | no | no | no | no | no | yes | invalid_decision | 3.8 |
| E | local-object | 1 | no | 5/5 | no | yes | no | no | no | yes | length_limited | 113.6 |
| E | local-object | 2 | no | 5/5 | no | yes | no | no | no | yes | timeout | 124.4 |
| E | local-object | 3 | no | 5/5 | no | yes | no | no | no | yes | timeout | 125.8 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 7.3 / 7.2 / 7.5 |
| A | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 4.6 / 3.0 / 4.9 |
| A | local-object | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 3.7 / 3.7 / 6.0 |
| A | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 11.8 / 9.1 / 13.6 |
| B | anthropic | 3 | 0 | 0/3 | 0/3 | 2/3 | 0 | 0 | 0 | 0 | 24.7 / 22.2 / 30.4 |
| B | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 2.5 / 2.5 / 2.6 |
| B | local-object | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 1 | 2 | 19.1 / 15.7 / 127.6 |
| B | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 21.5 / 20.4 / 21.9 |
| C | anthropic | 3 | 0 | 1/3 | 1/3 | 1/3 | 0 | 0 | 0 | 2 | 25.0 / 14.5 / 64.9 |
| C | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.5 / 0.4 / 4.8 |
| C | local-object | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 0 | 0 | 3 | 80.1 / 40.9 / 175.3 |
| C | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 63.8 / 60.3 / 73.8 |
| D | anthropic | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 6.6 / 3.3 / 7.7 |
| D | local | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 1.8 / 1.0 / 4.5 |
| D | local-object | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 2.2 / 2.1 / 9.6 |
| D | openai | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 14.7 / 14.4 / 23.1 |
| E | anthropic | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 9.0 / 3.8 / 10.8 |
| E | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 1.4 / 1.3 / 1.4 |
| E | local-object | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 2 | 1 | 0 | 124.4 / 113.6 / 125.8 |
| E | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 10.0 / 7.8 / 10.0 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
