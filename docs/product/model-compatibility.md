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

## Provider profiles (item 18)

`--provider-profile` now also takes `lm-studio`, `ollama`, `llama-cpp`, `vllm` and `openrouter` beside
`openai`, `anthropic` and `local`. Each sets the token-limit field, temperature, structured-output mode
and (for these five) a call timeout (300 s for the machine-local servers, 120 s for vLLM and OpenRouter);
any explicit flag or variable still wins. **Only `local` (LM Studio) was measured in this repository.** The
other five are written from each server's documentation and have not been run against a live server here;
their notes say so. `llama-cpp` defaults to `json-object` because llama-cpp-python answers `json_schema`
with HTTP 500. A profile never selects the endpoint.

## Bring your own model call (item 19)

For a program that embeds this crate: `WorkRuntime::with_provider(model, identity)` and
`service::serve_with(args, environments, Some(runtime))` take a host-supplied `ModelProvider`. The host's
gateway holds the credential; the agent loop only sees requests and replies, and a reply is validated like
any other. The host owns the delivery certainty it reports (return `FxError::Inference` with a `Delivery`).
For a hosted gateway over HTTP, no adapter is needed: point `--endpoint` at it and name its token with
`--api-key-env`. Not provided: a CLI plug-in mechanism (an executable or WASM adapter); that would be a new
FX provider and was not asked to be guessed at.
