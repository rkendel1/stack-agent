# Running against any OpenAI-compatible server

Three settings, each explicit, reported in the work report with its source, and none inferred from a
model name, an endpoint or an error.

## Non-streaming (`--streaming off`, `CHIP_STREAMING=off`)

Asks the server for one complete JSON response (`"stream": false`, no `stream_options`) instead of
server-sent events. The response is read whole (8 MiB cap) and decoded strictly: a provider error, a
tool call, a body that is not a chat-completions object, or a missing `finish_reason` is a malformed
response and delivers nothing. `finish_reason: length` is a typed failure exactly as it is on the stream.
OpenAI-compatible endpoints only; `off` is refused for the Anthropic provider.

## Response formats (`--structured-output`)

Adds `json-object`: a real `response_format: {"type":"json_object"}` (any JSON object, no schema), for
servers that reject `json_schema`. The existing `object` mode still sends `json_schema` with a bare
`{"type":"object"}` schema; the two are different requests. Chip validates every reply strictly in every
mode, so a weaker format costs coverage and recovery turns, never authority.

## The ladder (`--structured-output-fallback on`, `CHIP_STRUCTURED_OUTPUT_FALLBACK=on`; default off)

If the server rejects the structured-output request itself, the work steps down one rung and resends:
configured mode, then `json-object` (OpenAI-compatible only), then `none`.

- Only an HTTP answer that rejects the request counts: 400, 404, 415, 422, 500, 501. Timeouts,
  transport failures, 401/403, 429, 503 and a reply that arrived and was bad never step down. A genuinely
  transient 500 is indistinguishable from llama-cpp-python's schema 500 by status; it costs a weaker
  format for the rest of the work and is reported, not hidden.
- Sticky and bounded: a rung is never re-climbed, so a work makes at most `rungs - 1` extra requests.
- Only the request format changes. The model, endpoint and provider are the ones chosen. This is not a
  retry of the same request, a model fallback or a repair.
- Observable: a note on stderr per step, and `structured_output_fallback` in the report
  (`rungs`, `used`, and each step's `from`, `to`, `status`, `reason`).

It is off by default because AGENTS.md (Invariant 7) allows no silent substitution; turning it on is the
operator's explicit act. Making it the default for loopback endpoints is a one-line policy change if you
decide the evidence supports it.

## Credentials (`--api-key-env NAME`, `--no-ambient-keys`)

`--api-key-env NAME` reads the key from exactly that variable (unset is an error). Without it,
`CHIP_API_KEY` is used if set; otherwise the provider's own variable (`OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`) is used for a remote endpoint but **never** for an endpoint on this machine
(`localhost`, `127.0.0.0/8`, `::1`), and never with `--no-ambient-keys`. The report's
`credential_source` names the variable (or `none`), never the key.

## Tested

Mocked transport only (`tests/structured_fallback.rs`, `tests/credential_source.rs`,
`chip-fx-native/tests/native.rs`): they show what the product sends and does with each answer. They do
not show how llama-cpp-python, Ollama, LM Studio or vLLM behave; that needs a live run against each.
