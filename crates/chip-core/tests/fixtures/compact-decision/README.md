# Replies a small local model produced (or is reported to have produced)

These pin that the control boundary rejects them under the compact prompt exactly as under the full one.

- `woodcock-extra-closing-brace.reply.txt`: **a real capture.** The second reply of a baseline run of
  `stack-agent work --kind inspect` against `woodcock-qwen3-4b` (2026-10-23, request body logged by
  `audit/compact-prompt/proxy.py`). Correct JSON for `project.read` with one closing brace too many.
  It must be rejected as text after the JSON object, never trimmed.
- `woodcock-invented-capability.reply.txt`: **not a capture.** Reconstructed from a description of an
  earlier diagnostic ("invented the capability `inspect_cargo_toml` and incorrect input fields"); the
  exact original text was not kept. It is a syntactically valid decision that must be rejected because
  the capability is not offered, and never acted on.
