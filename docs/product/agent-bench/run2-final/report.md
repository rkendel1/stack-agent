# Agent effectiveness benchmark

Schema `chip.agent-bench.report.v1`, run `1792431580`.

## Trials

| Task | Provider | Trial | Provider completed | Decision accepted | Execution passed | Evidence complete | Correct | Complete | Safe | Failure | Latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | local | 1 | yes | yes | yes | yes | yes | yes | yes | none | 5.3 |
| A | local | 2 | yes | yes | yes | yes | yes | yes | yes | none | 3.0 |
| A | local | 3 | yes | yes | yes | yes | yes | yes | yes | none | 3.0 |
| A | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 11.1 |
| A | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 8.2 |
| A | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 7.8 |
| A | anthropic | 1 | yes | no | yes | yes | no | no | yes | invalid_decision | 9.8 |
| A | anthropic | 2 | yes | no | yes | yes | no | no | yes | invalid_decision | 9.2 |
| A | anthropic | 3 | yes | no | yes | yes | no | no | yes | invalid_decision | 10.3 |
| B | local | 1 | yes | no | yes | no | no | no | yes | invalid_decision | 8.0 |
| B | local | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 5.3 |
| B | local | 3 | yes | no | yes | no | no | no | yes | invalid_decision | 5.3 |
| B | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 19.9 |
| B | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 18.7 |
| B | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 15.5 |
| B | anthropic | 1 | yes | no | yes | yes | no | no | yes | invalid_decision | 27.9 |
| B | anthropic | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 35.3 |
| B | anthropic | 3 | yes | no | yes | yes | no | no | yes | invalid_decision | 26.0 |
| C | local | 1 | no | no | yes | yes | no | no | yes | timeout | 144.9 |
| C | local | 2 | no | no | yes | yes | no | no | yes | timeout | 187.0 |
| C | local | 3 | no | no | yes | yes | no | no | yes | timeout | 152.4 |
| C | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 87.1 |
| C | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 46.9 |
| C | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 52.0 |
| C | anthropic | 1 | yes | yes | yes | yes | yes | yes | yes | none | 58.9 |
| C | anthropic | 2 | yes | no | yes | no | no | no | yes | invalid_decision | 25.1 |
| C | anthropic | 3 | yes | yes | yes | yes | yes | yes | yes | none | 64.4 |
| D | local | 1 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 5.1 |
| D | local | 2 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.3 |
| D | local | 3 | yes | no | yes | n/a | n/a | yes | yes | invalid_decision | 2.3 |
| D | openai | 1 | yes | yes | yes | n/a | n/a | yes | yes | none | 16.9 |
| D | openai | 2 | yes | yes | yes | n/a | n/a | yes | yes | none | 16.9 |
| D | openai | 3 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 19.2 |
| D | anthropic | 1 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 4.9 |
| D | anthropic | 2 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 3.7 |
| D | anthropic | 3 | yes | yes | yes | n/a | n/a | yes | yes | blocked | 5.0 |
| E | local | 1 | yes | no | no | no | no | no | yes | invalid_decision | 3.0 |
| E | local | 2 | yes | no | no | no | no | no | yes | invalid_decision | 0.8 |
| E | local | 3 | yes | no | no | no | no | no | yes | invalid_decision | 0.8 |
| E | openai | 1 | yes | yes | yes | yes | yes | yes | yes | none | 11.1 |
| E | openai | 2 | yes | yes | yes | yes | yes | yes | yes | none | 9.3 |
| E | openai | 3 | yes | yes | yes | yes | yes | yes | yes | none | 9.8 |
| E | anthropic | 1 | yes | yes | yes | yes | yes | yes | yes | none | 8.3 |
| E | anthropic | 2 | yes | yes | yes | yes | yes | yes | yes | none | 9.4 |
| E | anthropic | 3 | yes | yes | yes | yes | yes | yes | yes | none | 9.6 |

## Aggregates

Rates use every attempted trial as the denominator (failed and timed-out trials included); skipped trials are listed apart. `n/a` is not measured, which is not zero.

| Task | Provider | Attempted | Skipped | Complete | Correct | Evidence | Safety violations | Timeouts | Token-limit | Other failures | Median / min / max latency (s) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A | anthropic | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 0 | 0 | 3 | 9.8 / 9.2 / 10.3 |
| A | local | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 3.0 / 3.0 / 5.3 |
| A | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 8.2 / 7.8 / 11.1 |
| B | anthropic | 3 | 0 | 0/3 | 0/3 | 2/3 | 0 | 0 | 0 | 3 | 27.9 / 26.0 / 35.3 |
| B | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 5.3 / 5.3 / 8.0 |
| B | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 18.7 / 15.5 / 19.9 |
| C | anthropic | 3 | 0 | 2/3 | 2/3 | 2/3 | 0 | 0 | 0 | 1 | 58.9 / 25.1 / 64.4 |
| C | local | 3 | 0 | 0/3 | 0/3 | 3/3 | 0 | 3 | 0 | 0 | 152.4 / 144.9 / 187.0 |
| C | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 52.0 / 46.9 / 87.1 |
| D | anthropic | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 4.9 / 3.7 / 5.0 |
| D | local | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 2.3 / 2.3 / 5.1 |
| D | openai | 3 | 0 | 3/3 | 0/0 | 0/0 | 0 | 0 | 0 | 0 | 16.9 / 16.9 / 19.2 |
| E | anthropic | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 9.4 / 8.3 / 9.6 |
| E | local | 3 | 0 | 0/3 | 0/3 | 0/3 | 0 | 0 | 0 | 3 | 0.8 / 0.8 / 3.0 |
| E | openai | 3 | 0 | 3/3 | 3/3 | 3/3 | 0 | 0 | 0 | 0 | 9.8 / 9.3 / 11.1 |

Cost is not reported: token counts are in each trial record where the provider reported them, and current pricing was not verified.
