# Token usage: per-call accounting, the token report, and prompt caching

What one `work` run spent on model input and output, call by call, and the one change made to
reduce resent context. Nothing here prices tokens, and nothing here claims a saving: savings are
only what a controlled comparison's provider-reported usage shows.

## The inference path

| Step | Where | What it does |
|---|---|---|
| Decision loop | `crates/chip-core/src/work.rs` `call_model` | Builds the context with the work's `EscalationContextPolicy` (`dedup-v1` for change, verify and act; `inspect-bounded-v1` for inspect), measures it (`ContextMetrics`), records `ModelEscalation`, calls the model, records `ModelCalled` with the provider's usage. |
| Messages | `crates/chip-core/src/lib.rs` `model_turn` | One system message with the goal and the question (the reply contract and the capability list), one system message per observation in order, one user message last with the state, prior decisions, ruled-out capabilities and any rejection. Nothing earlier is rewritten under `dedup-v1` unless an identical later observation lets an earlier one be omitted. |
| FX mapping | `crates/chip-fx/src/mapping.rs` | Chip messages to FX messages; FX usage back to Chip usage, field for field (`usage_of`). |
| Wire | `crates/fx-native-provider/src/anthropic.rs`, `openai.rs` | Anthropic: every system message goes into the top-level `system`; the request's `messages` is the one user turn. OpenAI-compatible: consecutive system messages are joined into one. |
| Usage | the same files' decoders | Anthropic: `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` (`message_start`, updated by `message_delta`), `output_tokens`. OpenAI: `prompt_tokens`, `prompt_tokens_details.cached_tokens`, `completion_tokens`, `completion_tokens_details.reasoning_tokens`. |
| Report | `crates/stack-agent/src/software_work.rs` (`context`, `usage`), `token_usage.rs` (`token_usage`) | The work report (`work --json`). |

## Per-call records (`token_usage`, schema `stack-agent.token-usage.v1`)

`work --json` reports carry a `token_usage` section unless `CHIP_TOKEN_DIAGNOSTICS=off`. It is
measured by a wrapper around the work's model provider, outermost, so it sees exactly what Chip
sent and received. Per call:

* `call`, `started_unix_ms`, `elapsed_ms`, `gap_before_ms` (from the previous call's end: tool
  execution and runtime work), `model_requested`, `max_output_tokens`;
* `outcome` (`response` or `failed`) and, for a failure, FX's `kind`, `delivery`, `status` and
  `partial_output_bytes`; `decision`, what the call led to (`request <capability>`, `complete`,
  `block`, `escalate`, `rejected`, `none`), never reason text;
* `request`: messages and bytes by role, `content_bytes` (the same measure as
  `context.calls[].request_bytes`), `serialized_messages_bytes_local` (a compact JSON rendering of
  the messages: a local size, not the provider's wire body), and `resent_from_previous_call`:
  messages byte-identical to one of the previous request, compared by SHA-256 fingerprints held
  only in memory;
* `usage`: `source` (`provider` or `unavailable`), `reported` (every field as the provider sent
  it, `null` when it did not), and the derived `uncached_input_tokens`, `total_input_tokens`,
  `total_tokens`.

`totals` and `by_model` sum each field over the calls that reported it, with `calls_known` and
`calls_unknown`. `run.labels` holds `CHIP_RUN_LABELS` (`key=value,...`, for example
`course=...,obstacle=...`), validated and recorded as given. The build identity is the report's
`build`. `observations` holds context-growth diagnostics: output bytes by capability, the largest
outputs, repeated reads of one file, identical outputs and output lines already seen, all by
fingerprint. No message text, reply text, path, query, key or header is recorded.

### Accounting rules

| Provider | Input field | Cache read | Cache write | Total input | Uncached input |
|---|---|---|---|---|---|
| Anthropic (`cache_separate`) | `input_tokens`, excludes both cache counts | `cache_read_input_tokens` | `cache_creation_input_tokens` | input + read + write | `input_tokens` |
| OpenAI-compatible (`cache_included`) | `prompt_tokens`, includes cached | `cached_tokens` | none (`null`) | `prompt_tokens` | prompt - cached |

A derived value is `null` when any field it needs is `null`. Reasoning tokens are part of the
output and are never added to it. Reports from builds before this change have
`cache_write_tokens: null` for every Anthropic call because the decoder did not read
`cache_creation_input_tokens`; their total input is therefore not derived.

## The token report

```
stack-agent token-report REPORT.json [--json] [--label KEY=VALUE]...
```

Reads a finished `work --json` report and calls nothing. From `token_usage` when present, otherwise
from `context.calls` (every report since per-call context measurement). It gives totals, mean and
median input per call, the largest calls, cumulative input by call, request size over time, context
growth and its largest steps, tool output diagnostics, inference versus between-call time, failed
and rejected calls, data-quality warnings, what the report cannot say (`unavailable`), and ranked
opportunities with impact, confidence and risk. Impact is an upper bound in input tokens where the
recorded counts support one and `null` where they do not. `carried_input_upper_bound` is computed
only when the counts show every request extended the previous one. Exit 0, 2 (usage), 3 (cannot
read the report).

## Prompt caching (`--prompt-cache on|off`, `CHIP_PROMPT_CACHE`)

On by default for the Anthropic provider. The system text is sent as one text block per system
message, each after the first starting with the same `\n\n` the joined string used, so the text
the model reads is unchanged. The last block carries `cache_control: {"type": "ephemeral"}`.
Because the instructions and observations come first and each request appends to them, the
blocks of one request are, byte for byte, the start of the next request's, which is the prefix a
provider cache can match. The per-turn user message stays after the breakpoint and is not cached.

* `off` sends the request exactly as before (one joined `system` string, no `cache_control`).
* For OpenAI-compatible endpoints there is no request field: the setting reports `false`, source
  `provider`; `on` is refused. A server that caches prefixes does so on its own and reports it
  as `cached_tokens`.
* Only the provider's reported `cache_read` and `cache_write` tokens say what was cached. Cache
  writes are billed above the ordinary input rate and reads below it. When a context policy
  removes or condenses an earlier observation, the prefix changes and the next request writes
  again. An endpoint that speaks the Messages API but rejects `cache_control` needs `off`.
