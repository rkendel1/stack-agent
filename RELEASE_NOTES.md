# Release notes

## Unreleased

* AppPort capabilities can be pinned to the contract the operator reviewed:
  `--appport-pin NAME@VERSION=sha256:DIGEST` (or `pins` in the capability file) offers a capability only
  under that AppPort `capabilityDigest`, and re-checks the host's manifest before every call; a changed
  contract is refused with nothing sent. `--appport-require-pins` withholds unpinned capabilities. The
  report and each observation now name the origin, version and digest. Unpinned behaviour is
  unchanged. `serde_json` now parses floats with `float_roundtrip`, so digests match `@appport/protocol`.
  See `docs/product/host-capabilities.md`.
* Added `stack-agent doctor`: diagnoses the model setup, the endpoint (a connection only, no request),
  `git`, `pax`, the project and the state directory, without calling the model or changing anything.
  See `docs/product/release.md`.
* Added a release pipeline (`.github/workflows/release.yml`), a portable packaging script that makes
  byte-identical archives from the same executable, and `scripts/install.sh`, which verifies the
  archive's SHA-256 before extracting it. No release has been published yet, and the executable is
  not code-signed or notarized.

## First Rust release

Rust Chip with the authoritative Rust FX is the agent runtime. The Zig agent is not retired by this file; this
is what the Rust release does and does not do.

### Read before you retry

**RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS.** Rust Chip does not guarantee exactly-once side effects, does
not resume an interrupted model response, and does not replay work automatically. After a crash or kill, a new
run starts over and may execute the same capability again. Persistence alone does not make an effect
idempotent. Callers that retry interrupted work must account for effects that may already have happened.

### Changed

* FX is the only inference executor. `--executor legacy` and `CHIP_EXECUTOR=legacy` fail before any request
  with `FX is the only supported executor`; `fx` is still accepted.
* Ollama's own `/api/chat` API is not supported. Use its OpenAI-compatible endpoint with
  `--provider openai-compatible`.
* A rejected decision no longer ends the work at once. That covers a reply that is not exactly one JSON object
  (narration, malformed JSON, two objects), an object that is not an allowed decision, a request for a capability the
  work does not offer, a request whose inputs its capability refuses, and a completion the observations do not
  support. Nothing in it is done; the model is told why (for a refused completion, what the goal still lacks or which
  answer rule the answer misses) and decides again, at most 2 rejections in a row
  (`WorkSpec::malformed_reply_limit`; `0` restores the strict behaviour). Limits, the deadline and the stuck guard are
  never reset. Provider failures and timeouts still end the work at once. See `docs/product/decision-contract.md`,
  "When a reply is rejected".
* Each model request now sends what does not change first: the goal and the reply contract, then the observations in
  order, then where the work stands. Each request begins with the whole of the one before it, so a provider's prefix
  cache can reuse it. With a 3B model on llama.cpp (CPU), 55-90% of input tokens were served from the cache instead of
  about none.
* FX sends a `json_schema` response format exactly as it was given, member order included. It used to sort object
  members by name, which made llama.cpp's grammar require `capability` before `decision` in a capability request, so
  a constrained model could not express one.
* `project.write` takes one bounded edit as well as a whole file. `replace` (at most 8 KiB, occurring
  exactly once) becomes `with` (at most 8 KiB), in a file of at most 256 KiB, always guarded by
  `expected_sha256`. A ranged `project.read` now reports the whole file's `file_sha256` so a large file
  can be edited. The edit goes through the same write path, checks and accounting as a whole-file
  write. "Not found", "ambiguous" and "too large" are observed failures with nothing written. The 32
  KiB whole-file limit is unchanged.

### Not supported (first release)

Permission modes, provider-native and parallel tool calls, silent tool continuation, pause/continue at the
step limit, image input, subagents / MCP / skills / memory / web tools, interactive approval, graceful
cancellation between a tool and the next request, publish-failure handling after the work ran, and any
idempotence guard. Each is refused or fails closed with an explicit message; none reports a completion. See
`docs/product/release-limitations.md`.

### Known limitations carried into this release

* The native provider does not support HTTP proxies or custom CA certificates (issue #35).
* No live OpenRouter evidence (waived for this migration; not a claim that it works).
* Anthropic and LM Studio live runs were reported clean by the owner; their raw output is not in the repository.
