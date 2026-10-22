# Release notes

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
* A model reply that is not exactly one JSON object no longer ends the work. Examples are narration around a decision,
  malformed JSON, or two objects. The reply is rejected with nothing in it done, the model is told why, and the work goes
  on, at most 2 such replies in a row (`WorkSpec::malformed_reply_limit`). Every other invalid decision, refused
  capability, provider failure and timeout still ends the work at once. See `docs/product/decision-contract.md`, "When a
  reply is rejected".
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
