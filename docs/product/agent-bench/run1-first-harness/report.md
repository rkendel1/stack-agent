# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792429845`.

## Trials

| Task | Provider | Trial | Provider completed | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local | 1 | yes | yes | yes | yes | yes | yes | yes | none | 79.1 |
| A | local | 2 | yes | yes | yes | yes | yes | yes | yes | none | 3.5 |
| A | local | 3 | yes | yes | yes | yes | yes | yes | yes | none | 3.1 |
| A | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 7.8 |
| A | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 11.0 |
| A | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 8.3 |
| A | anthropic | 1 | yes | no | yes | yes | no | no | yes | invalid_decision | 9.7 |
| A | anthropic | 2 | yes | no | yes | yes | no | no | yes | invalid_decision | 9.1 |
| A | anthropic | 3 | yes | no | yes | yes | no | no | yes | invalid_decision | 9.0 |
| B | local | 1 | no | no | yes | no | no | no | yes | timeout | 144.5 |
| B | local | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 15.1 |
| B | local | 3 | yes | no | yes | no | no | no | yes | invalid_decision | 16.1 |
| B | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 20.2 |
| B | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 17.4 |
| B | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 15.7 |
| B | anthropic | 1 | yes | no | yes | yes | no | no | yes | invalid_decision | 23.7 |
| B | anthropic | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 21.4 |
| B | anthropic | 3 | yes | no | yes | yes | no | no | yes | invalid_decision | 28.1 |
| C | local | 1 | no | no | yes | yes | no | no | yes | timeout | 269.0 |
| C | local | 2 | no | no | yes | yes | no | no | yes | timeout | 155.2 |
| C | local | 3 | no | no | yes | yes | no | no | yes | timeout | 152.1 |
| C | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 46.8 |
| C | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 45.2 |
| C | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 45.0 |
| C | anthropic | 1 | yes | yes | yes | yes | yes | yes | yes | none | 67.0 |
| C | anthropic | 2 | yes | yes | yes | yes | yes | yes | yes | none | 69.6 |
| C | anthropic | 3 | yes | no | yes | yes | no | no | yes | completion_refused | 63.3 |
| D | local | 1 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 11.0 |
| D | local | 2 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.2 |
| D | local | 3 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.2 |
| D | openai | 1 | yes | yes | yes | n/a | n/a | yes | yes | none | 19.7 |
| D | openai | 2 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 17.6 |
| D | openai | 3 | yes | yes | yes | n/a | n/a | yes | yes | none | 19.1 |
| D | anthropic | 1 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 4.4 |
| D | anthropic | 2 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 3.7 |
| D | anthropic | 3 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 4.1 |
| E | local | 1 | yes | no | no | no | no | no | yes | invalid_decision | 5.7 |
| E | local | 2 | yes | no | no | no | no | no | yes | invalid_decision | 0.8 |
| E | local | 3 | yes | no | no | no | no | no | yes | invalid_decision | 0.8 |
| E | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 10.1 |
| E | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 8.8 |
| E | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 9.8 |
| E | anthropic | 1 | yes | no | no | no | no | no | yes | invalid_decision | 2.5 |
| E | anthropic | 2 | yes | yes | yes | yes | yes | yes | yes | none | 9.1 |
| E | anthropic | 3 | yes | yes | yes | yes | yes | yes | yes | none | 9.1 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 0 | 0 | 3 | 9.1 / 9.0 / 9.7 |
| A | local | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 3.5 / 3.1 / 79.1 |
| A | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 8.3 / 7.8 / 11.0 |
| B | anthropic | 3 | 0 | 0/3 | 0/3 | 2/3 | 0 | 0 | 0 | 3 | 23.7 / 21.4 / 28.1 |
| B | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 1 | 0 | 2 | 16.1 / 15.1 / 144.5 |
| B | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 17.4 / 15.7 / 20.2 |
| C | anthropic | 3 | 0 | 2/3 | 2/3 | 3/3 | 0 | 0 | 0 | 1 | 67.0 / 63.3 / 69.6 |
| C | local | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 3 | 0 | 0 | 155.2 / 152.1 / 269.0 |
| C | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 45.2 / 45.0 / 46.8 |
| D | anthropic | 3 | 0 | 3/3 | 0/3 | 0/0 | 0 | 0 | 0 | 0 | 4.1 / 3.7 / 4.4 |
| D | local | 3 | 0 | 3/3 | 0/3 | 0/0 | 0 | 0 | 0 | 0 | 2.2 / 2.2 / 11.0 |
| D | openai | 3 | 0 | 3/3 | 0/3 | 0/0 | 0 | 0 | 0 | 0 | 19.1 / 17.6 / 19.7 |
| E | anthropic | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 9.1 / 2.5 / 9.1 |
| E | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.8 / 0.8 / 5.7 |
| E | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 9.8 / 8.8 / 10.1 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
