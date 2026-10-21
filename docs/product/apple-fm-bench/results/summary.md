## Layer A: basic inference (`fm respond`, greedy)

| Task | Successes | Categories | Median / P95 latency (s) |
|---|---|---|---|
| instruction | 3/3 | - | 0.2 / n<20 |
| list_format | 3/3 | - | 0.3 / n<20 |
| arithmetic | 0/3 | {'wrong_answer': 3} | 0.3 / n<20 |
| technical | 3/3 | - | 0.3 / n<20 |
| multistep | 3/3 | - | 0.3 / n<20 |
| ambiguous | 3/3 | - | 0.4 / n<20 |
| instruction_hierarchy | 0/3 | {'wrong_answer': 3} | 0.3 / n<20 |
| json_plain | 3/3 | - | 0.5 / n<20 |

Total 18/24.

## Layers B/C: decisions through the real Chip loop, arm `none`

| Kind | n | Response non-empty | Decision parses (boundary) | Correct judgment | Invocation accepted (executed) | Goal satisfied (verified) | Median / P95 latency (s) |
|---|---|---|---|---|---|---|---|
| select | 60 | 60/60 | 54/60 | 45/60 | 18/60 | 15/60 | 0.8 / 1.3 |
| no_tool | 6 | 6/6 | 6/6 | 6/6 | 0/6 | 0/6 | 1.6 / n<20 |
| adversarial | 9 | 9/9 | 9/9 | 6/9 | 0/9 | 0/9 | 0.8 / n<20 |
| ALL | 75 | 75/75 | 69/75 | 57/75 | 18/75 | 15/75 | 0.8 / 1.5 |

First-decision outcome: {'none': 57, 'invalid_decision': 6, 'wrong_judgment': 12}
First decision kinds: {'request:compute.hash': 9, 'invalid': 6, 'escalate': 9, 'complete': 6, 'request:compute.op_a': 12, 'request:compute.op_b': 21, 'request:compute.op_c': 12}
Executions of an undeclared capability: 0; reality without execution: 0

## Layers B/C: decisions through the real Chip loop, arm `schema`

| Kind | n | Response non-empty | Decision parses (boundary) | Correct judgment | Invocation accepted (executed) | Goal satisfied (verified) | Median / P95 latency (s) |
|---|---|---|---|---|---|---|---|
| select | 60 | 60/60 | 60/60 | 36/60 | 39/60 | 36/60 | 0.9 / 1.4 |
| no_tool | 6 | 6/6 | 6/6 | 6/6 | 0/6 | 0/6 | 1.2 / n<20 |
| adversarial | 9 | 3/9 | 3/9 | 3/9 | 3/9 | 3/9 | 120.0 / n<20 |
| ALL | 75 | 69/75 | 69/75 | 45/75 | 42/75 | 39/75 | 0.9 / 120.0 |

First-decision outcome: {'none': 45, 'wrong_judgment': 24, 'provider_error': 6}
First decision kinds: {'request:compute.hash': 9, 'complete': 21, 'request:compute.selftest': 3, 'request:compute.op_a': 6, 'request:compute.op_b': 12, 'request:compute.op_c': 12, 'escalate': 6, 'none': 6}
Executions of an undeclared capability: 0; reality without execution: 0

## Real runtime: `stack-agent --agent-bench`, local-none arm, apple

| Task | Attempted | Verified complete | First-reply contract valid | Failure kinds |
|---|---|---|---|---|
| A | 3 | 0/3 | 0/3 | {'invalid_decision': 3} |
| B | 3 | 0/3 | 0/3 | {'invalid_decision': 3} |
| C | 3 | 0/3 | 1/3 | {'invalid_decision': 2, 'completion_refused': 1} |
| D | 3 | 3/3 | 0/3 | {'invalid_decision': 3} |
| E | 3 | 0/3 | 3/3 | {'completion_refused': 3} |

## Real runtime: `stack-agent --agent-bench`, local-none arm, baseline

| Task | Attempted | Verified complete | First-reply contract valid | Failure kinds |
|---|---|---|---|---|
| A | 3 | 3/3 | 3/3 | {None: 3} |
| B | 3 | 1/3 | 3/3 | {None: 3} |
| C | 3 | 2/3 | 3/3 | {'timeout': 1, None: 2} |
| D | 3 | 3/3 | 3/3 | {None: 2, 'escalated': 1} |
| E | 3 | 0/3 | 3/3 | {'blocked': 3} |

