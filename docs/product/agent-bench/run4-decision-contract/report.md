# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792437493`.

## Trials

| Task | Provider | Trial | Provider completed | Replies schema-valid | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 5.2 |
| A | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.9 |
| A | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.9 |
| A | openai | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 13.0 |
| A | openai | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 12.6 |
| A | openai | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 12.0 |
| A | anthropic | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.2 |
| A | anthropic | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 6.9 |
| A | anthropic | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 7.1 |
| A | local-object | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 5.6 |
| A | local-object | 2 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 3.6 |
| A | local-object | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 3.6 |
| B | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.5 |
| B | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.5 |
| B | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 2.4 |
| B | openai | 1 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 24.4 |
| B | openai | 2 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 21.2 |
| B | openai | 3 | yes | 7/7 | yes | yes | yes | yes | yes | yes | none | 17.3 |
| B | anthropic | 1 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 24.9 |
| B | anthropic | 2 | yes | 7/7 | yes | yes | no | yes | no | yes | none | 21.2 |
| B | anthropic | 3 | yes | 8/8 | yes | yes | yes | yes | yes | yes | none | 25.0 |
| B | local-object | 1 | yes | 5/5 | no | yes | no | no | no | yes | invalid_decision | 10.5 |
| B | local-object | 2 | yes | 5/5 | no | yes | no | no | no | yes | invalid_decision | 8.1 |
| B | local-object | 3 | yes | 5/5 | no | yes | no | no | no | yes | invalid_decision | 8.1 |
| C | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 0.4 |
| C | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 0.3 |
| C | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 0.3 |
| C | openai | 1 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 56.1 |
| C | openai | 2 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 55.1 |
| C | openai | 3 | yes | 32/32 | yes | yes | yes | yes | yes | yes | none | 52.8 |
| C | anthropic | 1 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 61.7 |
| C | anthropic | 2 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 62.1 |
| C | anthropic | 3 | yes | 30/30 | yes | yes | yes | yes | yes | yes | none | 61.1 |
| C | local-object | 1 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 27.6 |
| C | local-object | 2 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 25.7 |
| C | local-object | 3 | yes | 31/31 | no | yes | yes | no | no | yes | completion_refused | 26.2 |
| D | local | 1 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 1.0 |
| D | local | 2 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 0.9 |
| D | local | 3 | yes | 1/1 | no | yes | n/a | n/a | yes | yes | completion_refused | 0.9 |
| D | openai | 1 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | none | 8.1 |
| D | openai | 2 | yes | 2/2 | yes | yes | n/a | n/a | yes | yes | blocked | 9.5 |
| D | openai | 3 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | blocked | 9.7 |
| D | anthropic | 1 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | blocked | 3.2 |
| D | anthropic | 2 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | blocked | 4.5 |
| D | anthropic | 3 | yes | 1/1 | yes | yes | n/a | n/a | yes | yes | escalated | 3.5 |
| D | local-object | 1 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 4.8 |
| D | local-object | 2 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.1 |
| D | local-object | 3 | yes | 4/4 | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.1 |
| E | local | 1 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.4 |
| E | local | 2 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.4 |
| E | local | 3 | yes | 1/1 | no | no | no | no | no | yes | completion_refused | 1.3 |
| E | openai | 1 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 11.0 |
| E | openai | 2 | yes | 6/6 | yes | yes | yes | yes | yes | yes | none | 12.5 |
| E | openai | 3 | yes | 5/5 | yes | yes | yes | yes | yes | yes | none | 8.6 |
| E | anthropic | 1 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 8.3 |
| E | anthropic | 2 | yes | 1/2 | no | no | no | no | no | yes | invalid_decision | 3.9 |
| E | anthropic | 3 | yes | 4/4 | yes | yes | yes | yes | yes | yes | none | 9.0 |
| E | local-object | 1 | no | 5/5 | no | yes | no | no | no | yes | length_limited | 108.4 |
| E | local-object | 2 | no | 5/5 | no | yes | no | no | no | yes | timeout | 123.9 |
| E | local-object | 3 | no | 5/5 | no | yes | no | no | no | yes | timeout | 125.3 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 7.1 / 6.9 / 7.2 |
| A | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 2.9 / 2.9 / 5.2 |
| A | local-object | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 3.6 / 3.6 / 5.6 |
| A | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 12.6 / 12.0 / 13.0 |
| B | anthropic | 3 | 0 | 2/3 | 3/3 | 2/3 | 0 | 0 | 0 | 0 | 24.9 / 21.2 / 25.0 |
| B | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 2.5 / 2.4 / 2.5 |
| B | local-object | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 8.1 / 8.1 / 10.5 |
| B | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 21.2 / 17.3 / 24.4 |
| C | anthropic | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 61.7 / 61.1 / 62.1 |
| C | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.3 / 0.3 / 0.4 |
| C | local-object | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 0 | 0 | 3 | 26.2 / 25.7 / 27.6 |
| C | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 55.1 / 52.8 / 56.1 |
| D | anthropic | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 3.5 / 3.2 / 4.5 |
| D | local | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 0.9 / 0.9 / 1.0 |
| D | local-object | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 2.1 / 2.1 / 4.8 |
| D | openai | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 9.5 / 8.1 / 9.7 |
| E | anthropic | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 8.3 / 3.9 / 9.0 |
| E | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 1.4 / 1.3 / 1.4 |
| E | local-object | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 2 | 1 | 0 | 123.9 / 108.4 / 125.3 |
| E | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 11.0 / 8.6 / 12.5 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
