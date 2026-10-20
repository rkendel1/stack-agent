# Chip over the native FX provider

`crates/chip-fx-native` serves Chip's model port (`chip_inference::ModelProvider`) with the authoritative FX
executor and a native Rust provider, `fx-native-provider` from `rkendel1/fx` (see `docs/rebuild/NATIVE_PROVIDER.md`
there). It is the Rust-only route for an embedder that has no Node host.

```
Chip work loop -> NativeModelProvider -> chip_fx::FxModelProvider -> fx_core::execute
                                       -> SingleAttemptCapability<OpenAiCompatible> -> one HTTPS request
```

There is no second executor. Request validation, the single attempt, normalization, cancellation, delivery
certainty and the terminal outcome are the same FX code the Node-backed path uses. One `complete` call is one FX
attempt and at most one HTTP request: no retry, no fallback, no model routing.

## Using it

```rust
// The user chose the provider, endpoint and model explicitly. Nothing here selects or falls back.
let selection = NativeSelection::new(ProviderKind::Anthropic);        // ANTHROPIC_API_KEY
let settings = NativeSettings::resolve(
    &selection,
    &EnvCredentials::process(),         // Chip's host layer reads the environment, once
    OptionsPolicy::Carry,               // send the token cap and temperature; FX refuses what a port cannot carry
    *b"chip-cli",
)?;                                      // missing key: a Configuration error, no request made
let provider = NativeModelProvider::new(settings)?;
let agent = Agent::new(Arc::new(provider)) /* ... */;
```

## Credentials: Chip acquires, FX consumes

```
CredentialSource  ->  resolved credential  ->  NativeSettings (FX provider configuration)
```

Chip's host layer (`credentials.rs` in this crate) is the only place that reads the environment; a test enforces it.
The native FX provider receives the one resolved key, or none, through its configuration and never inspects the
environment or a credential file.

| Provider | Variable | Notes |
| --- | --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` | always required |
| OpenAI-compatible | `OPENAI_API_KEY` | required unless the endpoint is a plain-`http` loopback address |
| OpenRouter | a variable the user names (`key_env`, for example `OPENROUTER_API_KEY`) | uses the OpenAI-compatible provider; attribution headers via `headers` |
| LM Studio | none | `http://localhost:1235/v1`: no key needed, and one is used if the variable is set |

Rules: a named variable that is unset is an error, never a silent fall back to another variable (an
`OPENAI_API_KEY` is not sent to OpenRouter because `OPENROUTER_API_KEY` is missing); blank values count as unset and
values are trimmed; a variable name must look like one; the error names the variable to set and says no request was
made. The key prints as `[redacted]` in `Debug`, is removed from provider messages, and is never part of an FX
request, so it cannot reach a session log. Headers the adapter owns (authorization, `x-api-key`, content type,
host, `anthropic-version`) are rejected when the provider is built.

An interactive prompt or the Easy-LLM vault can implement `CredentialSource` later without touching provider
execution. Neither exists today, deliberately: the vault would couple Rust Chip to its file format, encryption and
master-password lifecycle. The Node-backed path keeps using the vault and Easy-LLM's router unchanged.

The blocking network call runs on Tokio's blocking pool, so the async work loop is not stalled; the embedder needs a
Tokio runtime.

## Known limitations of the native provider

**HTTP proxies: supported through `CHIP_PROXY`, explicitly.** `CHIP_PROXY=http://host:port` sends model calls
through that HTTP `CONNECT` proxy, for both providers (chip-rs#35, implemented). The tunnel carries TLS end to end, so certificate
validation is unchanged; for a TLS-intercepting proxy, also set `SSL_CERT_FILE` to its CA (below).
`CHIP_PROXY_USERNAME` and `CHIP_PROXY_PASSWORD` (both or neither) authenticate to the proxy with Basic; the
password is never printed (errors, `Debug` output and the provider's failure text are scrubbed) and the endpoint
never receives it. `CHIP_NO_PROXY` is a comma-separated list of hosts reached directly (`example.com`,
`.example.com` or `*.example.com`). `stack-agent` reads only its own variables: `HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY` and `NO_PROXY` are **not** honored, so an inherited shell setting never redirects model traffic.
Refused before anything is sent, with an error that names the variable to fix and prints no value: a companion
variable without `CHIP_PROXY`; a username without a password; a proxy URL that is not `http://host:port` (no
`https://` or SOCKS proxy, no path, no credentials in the URL); credentials or a `CHIP_NO_PROXY` entry the provider
cannot use; and a keyed plain-`http` endpoint, whose key the proxy would see in the clear. A proxy that cannot be reached, or that
refuses the tunnel (for example `407`), is the model call's `transport` failure with its delivery certainty: there
is no direct connection as a fallback and no second executor. The proxy lives inside the FX provider boundary
(`fx-native-provider`). What is verified, all against loopback stand-ins: routing through a `CONNECT` proxy, the
no-proxy bypass and its matching rules, Basic credentials reaching only the proxy, a refused (`407`) or unreachable
proxy failing once with zero connections to the endpoint, and a proxy that echoes the credentials in its refusal
never getting them into any output (`crates/fx-native-provider/tests/proxy.rs` and `tests/transport_boundaries.rs`,
`crates/chip-fx-native/tests/proxy.rs` through Chip); the variable rules (`provider_selection.rs` tests); and the
same behaviour through the real binary, including that ambient proxy variables route nothing
(`crates/stack-agent/tests/transport_boundaries.rs`). An opt-in smoke test through a real proxy exists
(`crates/stack-agent/tests/live_smoke.rs`, `STACK_AGENT_LIVE_PROXY`). **Not verified:** a real corporate proxy (that
smoke test has not been run), and proxy authentication schemes other than Basic (not supported).

**Custom root certificates: supported through `SSL_CERT_FILE`, with limits.** If `SSL_CERT_FILE` names a PEM
file, `stack-agent` reads it (the host reads it; the provider never touches the filesystem or the environment) and
trusts **exactly** the certificates in it for `https`, in place of the bundled public roots, the way
`curl --cacert` does. Consequences to know: it applies to every `https` endpoint for that process, so the file
must contain the roots for every endpoint you use; a file that cannot be read is an error naming the variable
(`nothing was run`), never a silent fall back to the public roots, and so is a file with no valid certificate;
certificate validation stays on, and there is no option or variable that turns it off. Unset or empty keeps the
bundled roots. What is verified: the bundle is accepted or refused as described (unit tests, both providers) and
the variable is read and failed on as described. The TLS handshake is tested against `openssl s_server` with
generated certificates (`crates/fx-native-provider/tests/private_ca.rs` and `tests/tls_boundaries.rs`, and through
the real binary in `crates/stack-agent/tests/transport_boundaries.rs`): a server signed by a private CA is
reached with its CA, directly and through a proxy; it is refused with the bundled roots, with a different CA, as a
self-signed server, and under a name its certificate does not carry, each directly and through a proxy, and the
key never crosses the refused session.

Other limits, unchanged: plain `http` only for loopback (or a private IP literal with the explicit
`CHIP_ALLOW_INSECURE_LAN_HTTP=1` opt-in, keyless); no client certificates (mTLS); Ollama's own `/api/chat` API is not
supported (below).

## The executor in `stack-agent`

FX is the only executor. `stack-agent work`, `stack-agent serve`, `stack-agent "<prompt>"` and every other
model-calling command
(`verify`, `work_demo`, `horizon`, `pax_work`, the micro shadow, `--benchmark-live-reasoner`, the `micro_eval`
bench) build their provider through `provider_selection.rs`, which is the one place that constructs it:

```
stack-agent work "<goal>" --provider anthropic --model <model>
```

`--executor fx` and `CHIP_EXECUTOR=fx` are still accepted (they select the only executor), so a deployment
that set them keeps working. **`--executor legacy`, `CHIP_EXECUTOR=legacy` and any other value fail before any
provider request** with `FX is the only supported executor`; both the flag and the environment are checked,
so a valid flag does not hide a stale environment value. Nothing falls back to anything.

The same `CHIP_PROVIDER`, `CHIP_MODEL` and `CHIP_ENDPOINT` select the provider; the endpoint must be the full
URL (`.../v1/chat/completions` for OpenAI-compatible servers, `.../v1/messages` for Anthropic), from which the
API root is derived. Keys come from `CHIP_API_KEY` if set, otherwise `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`
(see Credentials above). Anything the native path cannot carry is a configuration error that names it, before
any request: `CHIP_ENABLE_THINKING` on the Anthropic provider (which has no such field), and an endpoint that
does not end in the expected path. `crates/chip-fx/tests/retirement.rs` fails if `chip-legacy-http` returns, is
imported, or is named by a script or workflow.

## Behavior of the FX executor

`crates/stack-agent/tests/fx_executor.rs` runs the executor against one loopback server that answers in the
format FX asks for (an event stream), and pins:

* the response text and token usage, for OpenAI-compatible and Anthropic;
* the wire fields `model`, `messages` (consecutive system messages merged into one), `max_tokens` and
  `temperature` (a configured `0.0` is sent, not dropped), Anthropic's top-level `system`, authentication
  (`Bearer`, `x-api-key`, `anthropic-version`) and the Anthropic workspace header and fallback cap (1024);
* JSON-object output as `response_format: json_schema` with `{"type":"object"}` (accepted by OpenAI and LM
  Studio; not verified on OpenRouter models) and that Anthropic ignores the flag;
* failures for 401, 403, 429, 500 and 503: their class, exactly one request (no retry), and the key never in
  the error; structured detail (kind, status, retry-after, partial output, delivery certainty
  `definitely_unsent` / `possibly_sent` / `confirmed_response`);
* a stream cut mid-way is a failure with the partial text, never a completion;
* what it refuses by name before any request (an endpoint without the provider path, `CHIP_ENABLE_THINKING` on
  Anthropic, a missing key, plain `http` off loopback);
* `CHIP_ENABLE_THINKING` as the top-level `chat_template_kwargs.enable_thinking` field on OpenAI-compatible
  endpoints.

## Release evidence still outstanding

* **Live checks.** Anthropic and LM Studio were run by the owner and reported clean; the raw output and audit have
  not been added to the repository. **OpenRouter has not been run** (a release gate, tracked separately).
  An OpenAI-compatible endpoint (Vercel AI Gateway) was reached by `fxlive` (`performance.md` section 8) and by
  the opt-in prompt-path smoke test `crates/stack-agent/tests/live_smoke.rs` (`STACK_AGENT_LIVE_PROVIDER=1`).
  Everything else above is mocked transport; TLS is tested against a local `openssl s_server` only.
* The horizon live run needs real Compute before it asks a model; its model request was not exercised end to end,
  only its provider selection.

## Ollama

Decision (2026-10-16, repository owner): **Ollama's own `/api/chat` API is not a supported provider** and no native
implementation is built. Ollama's OpenAI-compatible endpoint is the supported path:

```
stack-agent work --provider openai-compatible --endpoint http://127.0.0.1:11434/v1/chat/completions --model <model> "<goal>"
```

`CHIP_PROVIDER=ollama` (and `--provider ollama`, `CHIP_MICRO_PROVIDER=ollama`) fails before any request with a
configuration error that names this replacement; nothing falls back. `CHIP_OLLAMA_ENDPOINT` is no longer read.
Supporting `/v1/chat/completions` does not preserve native-API behavior: the former native adapter sent
`temperature` and `num_predict`, ignored `response_format`, and never set `num_ctx`. Anyone who needs those must say
so; the way back is a native adapter behind the FX provider boundary.

## Evidence

`crates/chip-fx-native/tests/native.rs` (9 cases) and `tests/credentials.rs` (13 cases, with a fake environment and no real keys: resolution, missing keys, LM Studio without authentication, OpenRouter headers and key, redaction, rejected headers) runs the real Chip work loop, the real FX executor and the
native provider against a loopback mock server: completion, HTTP error with status and `Retry-After`, no retry,
credential redaction, option refusal before any connection, budget timeout, mid-stream cancel with partial
output, a failed attempt ending the work with one provider call, and refused settings. The provider's own
conformance suite is `crates/fx-native-provider/tests/`; the Node-versus-native parity check lives in
`rkendel1/fx`. All of it is mocked transport (TLS only against a local `openssl s_server`); live runs are listed
under "Release evidence still outstanding".

## Ollama

Decision (2026-10-16, repository owner's default preference): **Ollama's own `/api/chat` API is not a supported provider** and no native implementation is built. Ollama's OpenAI-compatible endpoint is the supported path:

```
stack-agent work --provider openai-compatible --endpoint http://127.0.0.1:11434/v1/chat/completions --model <model> "<goal>"
```

`CHIP_PROVIDER=ollama` (and `--provider ollama`, `CHIP_MICRO_PROVIDER=ollama`) now fails before any request with a configuration error that names this replacement; nothing falls back. `CHIP_OLLAMA_ENDPOINT` is no longer read. Supporting `/v1/chat/completions` does not preserve native-API behavior: the native adapter sent `temperature` and `num_predict` and ignored `response_format`, and never set `num_ctx`. Anyone who needs those must say so; the way back is a native adapter behind the FX provider boundary, not the legacy executor.

## Replies that stop at the output cap

A reply the provider ends with `finish_reason: "length"` is partial by definition. FX reports it as the typed failure
`length_limited` (delivery `confirmed_response`: the provider answered, and the answer is not whole), carrying the text
only as `partial_output`. Chip never parses it, whatever it contains, so it cannot become a decision, an execution, an
answer or grounding; the work ends `failed`, the audit stays clean, and nothing is retried. `chip work --json` lists every
failed model call, typed, under `model_failures` (`kind`, `status`, `delivery`); a reply that finished (`stop`) but is
not a valid decision is not a model failure and appears only in `outcome_reason`. A transport or HTTP failure is another
kind (`provider_unavailable`, `timeout`, ...), never `length_limited`, and a context-budget refusal is a separate
`context_limit_rejections` count. `crates/stack-agent/tests/incomplete_response.rs` pins this through the real CLI, FX
and native adapter against a mock server.

## Work generation settings

`chip work` (and `chip serve`, which prepares the same runtime) sends four generation settings with every model
request. Each has one effective value and one source, resolved in this order, setting by setting:

1. the command-line flag,
2. the environment variable (the work-specific configuration),
3. the application default.

| Setting | Flag | Variable | Default | Accepted |
| --- | --- | --- | --- | --- |
| Output tokens per reply | `--max-output-tokens N` | `CHIP_MAX_OUTPUT_TOKENS` | `2048` | whole number, 1 to 131072 |
| Temperature | `--temperature T` | `CHIP_TEMPERATURE` | `0.0` | 0 to 2, at most three decimals |
| Structured output | `--structured-output M` | `CHIP_STRUCTURED_OUTPUT` | `object` | `object` or `none` |
| Call timeout | `--call-timeout-secs N` | `CHIP_CALL_TIMEOUT_SECS` | `30` | whole seconds, 1 to 86400 |

An invalid value is a usage error (exit 2) that names the flag or variable, before anything is selected or sent. Nothing
is clamped or rounded: a value is used as given or refused. Blank variables count as unset. A server or provider that
accepts less than a value (a lower token cap, a narrower temperature range) refuses the request, and that refusal is
reported as the model failure it is.

`object` sends `response_format: {"type":"json_schema", "json_schema": {"schema": {"type":"object"}, "strict": false}}`.
`none` omits the field. It does not become `json_object` (which the tested server rejects with HTTP 400) or any other
format, and no mode is chosen or changed by a failure: a failed request is never repeated with other settings. The
Anthropic provider carries a schema as `output_config.format` and only for `schema-closed`; any other default mode is reported as `none` with source `provider`, and any other explicit mode is refused.

Structured output is a request to the server; it is not validation. Whatever the mode, Chip parses the reply strictly
(one JSON object, one decision), checks the decision against the declared capabilities and inputs, authorizes and
executes it, and for an inspection checks that the answer cites a file it observed. A reply cut off at the token cap is
the typed failure `length_limited` under both modes.

Temperature and the token cap are set by `GenerationPolicy`, a wrapper `WorkRuntime::prepare` puts around the provider
it builds, so what the report calls effective is what the provider is handed. `ProviderConfig::max_tokens` and
`temperature` are not part of this order: they only fill a request that carries none, and Chip's requests always carry
both. Experiments that pin their own settings (the micro model, the benchmarks) build their own providers and never pass
through the policy, so no operator setting reaches them; a test enforces it.

Every `chip work --json` report carries the effective settings, and `model_failures` for any failed model call:

```json
"generation": {
  "max_output_tokens": {"value": 8192, "source": "flag:--max-output-tokens"},
  "temperature":       {"value": 0.0,  "source": "default"},
  "structured_output": {"value": "none", "source": "environment:CHIP_STRUCTURED_OUTPUT"},
  "call_timeout_secs": {"value": 120, "source": "flag:--call-timeout-secs"}
},
"model_failures": [{"kind": "length_limited", "status": null, "delivery": "confirmed_response"}]
```

Sources are `default`, `flag:<flag>`, `environment:<variable>` or `provider`. The token cap and temperature are set on
every request that reaches the provider; the structured-output mode decides whether `response_format` is in the body. The
tests in `tests/work_generation.rs` compare each of the three with the body a mock server actually received. The
reported timeout is the limit each call was given, not how long any call took (`measurement.total_latency_ms` is the
elapsed time).

### The call timeout

The timeout is one wall-clock deadline for a whole provider attempt, not a generation deadline. In the native provider
it is `timeout_global` plus an executor-side check: it covers connecting, sending, the server's own queueing and prompt
processing, and reading the entire streamed reply. (Connecting also has its own, shorter limit.) It reaches the provider
as the `AttemptBudget` of every request that provider makes, so it is a property of the provider one `WorkRuntime`
builds and nothing is shared between runs with different values. FX refuses a zero budget or one over 24 hours, which
is why `0` and values above 86400 are refused by name before anything is sent. Callers that do not configure it keep
30 s. It is not `--deadline-secs`, which bounds the whole work.

A call that outlasts it is the typed failure `timeout`. If nothing came back first, delivery is `possibly_sent` (the
request may have reached the server, so it is not replayed); if bytes had begun to arrive, `confirmed_response`. Either
way the partial text is dropped: a complete-looking decision in a stream that never finished is not a decision, no
capability runs, and nothing is retried. `timeout`, `length_limited`, an HTTP error, a malformed reply and a
context-budget refusal are five different outcomes in the report. A longer timeout lets a slow model finish; it does
not make a model that loops stop.

### Known incompatibility: `google/gemma-4-26b-a4b-qat` behind LM Studio (measured 2026-10-19)

The served id is `…-a4b-qat`; `…-a4-qat` is not a model this server lists. Chip's final-answer request (Chip's
`json_schema` `{"type":"object"}` response format, `temperature: 0.0`, `max_tokens: 2048`) was replayed one variable at
a time. All runs below use the same request:

| Change | Result |
| --- | --- |
| none | runs to the cap: ~10 KB of one repeated phrase inside the `summary` string, 0 reasoning tokens, `finish: length` |
| `max_tokens` 8192 | still loops, to 8191 tokens |
| `response_format` strict schema (`required`, `additionalProperties:false`) | same loop, byte for byte |
| `temperature` 0.1 / 0.3 | loops 1 of 1 / 2 of 4 runs; the others finish with valid JSON in 190–360 tokens |
| a minimal 40-word prompt, same format | loops at 0.0 and at 0.3: Chip's context is not the trigger |
| no `response_format` | the model reasons first (2000+ tokens) and exhausts 2048 with 222 characters of content; `chat_template_kwargs.enable_thinking=false` changes nothing on this server |
| no `response_format`, `max_tokens` 8192, full request | completes: 4519 tokens (4358 reasoning), contract-valid JSON |
| `response_format: json_object` | HTTP 400: the server accepts only `json_schema` |

So under the structured-output constraint the model can fail to stop, and without it the model needs a budget of
several thousand tokens for hidden reasoning. Neither is a Chip request-formatting defect, and neither is something the
runtime can repair: a configuration that works for this model (no schema, a large output budget) trades the structured
contract for reasoning room and needs an explicit operator setting, which is a separate design decision. Measured by
replaying one captured request against the server with a throwaway script; the body contains repository text and is not committed.

### Measured with the new settings (2026-10-19, same task, model and limits, `--deadline-secs 180`)

The served model `google/gemma-4-26b-a4b-qat` was used under the name the server lists, and the CLI sent that name.

| Run | Output tokens / temp / format | Final call | Grounded answer |
| --- | --- | --- | --- |
| A | 2048 / 0.0 / `object` | `length_limited`, repetition loop | no |
| B | 8192 / 0.0 / `object` | provider `timeout` (30 s) while still looping | no |
| C1 | 8192 / 0.0 / `none` | provider `timeout` (30 s) | no |
| C2 | 8192 / 0.0 / `none` | provider `timeout` (30 s) | no |
| C3 | 8192 / 0.0 / `none` | complete, valid `complete` decision (2021 tokens) | no: Chip refused it, the summary cites no observed file path |

`none` removes the repetition loop in these samples, but the model then spends its budget on reasoning, and two of three
final calls outlasted the fixed 30 s call timeout. The one reply that arrived whole was refused by the existing
grounding check. One refused sample is not a reliable configuration. `none` is an experimental compatibility setting for
evaluating models, not a fix: it removes a server-side constraint and leaves everything Chip checks in place, and this
incompatibility is not solved.

### Measured with the call timeout (2026-10-19, `--deadline-secs 180`, same task, model and limits)

The previous table is not comparable to this one. Between the two, inspections stopped offering `project.write` and
`pax.test`, which shortens the capability list in the prompt, and the model behaved differently. Same binary, same
settings as run A, with and without that list:

| Capability list | Runs | Result |
| --- | --- | --- |
| previous (10 capabilities) | 3 | `length_limited` on the final call, 3 of 3 |
| current (8 capabilities, the inspect allowlist) | 3 | completed and grounded, 3 of 3, 5 model calls, 3 reads, 6 to 23 s |

So configuration A now completes, with the default 30 s timeout, and the cause is the shorter prompt, not the timeout:
no call in A or B came near 30 s. Why the shorter list helps is not established; one cause was not isolated from the
other.

| Run | Tokens / temp / format / timeout | Result | Grounded |
| --- | --- | --- | --- |
| A, 3 runs | 2048 / 0.0 / `object` / 30 s | completed, 5 calls, 23 s, 8 s, 6 s | yes |
| B, 2 runs | 8192 / 0.0 / `object` / 120 s | completed, 5 calls, 5 s, 6 s | yes |
| C, 3 runs | 8192 / 0.0 / `none` / 120 s | 1 completed (7 calls, 108 s, last reply 5811 tokens); 2 refused by Chip (complete answer, cites no observed path) | 1 of 3 |
| C, 3 runs | 8192 / 0.0 / `none` / 30 s | all 3 refused by Chip for the same reason; none timed out | 0 of 3 |

The timeout did not decide any outcome here. C at 30 s did not time out; the one C run with a very long reply (5811
tokens, which at this server's speed is plausibly over 30 s) happened at 120 s, and the 30 s runs took a different path,
so whether 30 s would have stopped it is not shown. Decoding at temperature 0.0 on this server is not repeatable: runs
with identical settings diverged at the fifth call.

A grounded answer is one that cites a file Chip observed. A's answer cites `Cargo.toml`, `README.md` and
`crates/stack-agent/Cargo.toml` and meets that contract, but it is thin: it read one crate manifest of the 28 members,
says the members include `crates/chip-*` and `crates/fx-*` (those are patterns in a comment; the manifest lists each
member by name), and says `stack-agent` depends on `fx-*` crates (it does so only for dev-dependencies). It is grounded,
not verified, and not a complete answer to the task's request to cover the major crates.

### Hosted models and the request fields they accept (measured 2026-10-19)

`gpt-5.5-2026-04-23` rejects `max_tokens` and any non-default temperature; `claude-sonnet-5-5` rejects `temperature`.
Chip sends both by default, so each needs an explicit setting: `--token-limit-field max_completion_tokens` and
`--temperature omit` for the former, `--temperature omit` for the latter (see `docs/product/agent-bench.md`). The field
name is a host choice in the OpenAI-compatible adapter (`OpenAiConfig::with_max_completion_tokens`); the Messages API
always uses `max_tokens`, so the option is refused for the Anthropic provider.
