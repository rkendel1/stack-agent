# Task (frozen before the run)

Problem: Anthropic reports prompt-cache write tokens as `cache_creation_input_tokens` in the usage object of the
streamed `message_start` event. The native Anthropic provider path never carries it into FX `Usage.cache_write_tokens`
(documented gap: docs/known-issues.md section K, "Not claimed"). Affected: crates/fx-native-provider (wire parser, tests)
and crates/fx-single-attempt (PortUsage and its mapping into FX Usage).

Goal text submitted verbatim (does not name files):
"Anthropic reports prompt-cache write tokens as `cache_creation_input_tokens` in the usage of the streamed
`message_start` event, but the native Anthropic provider path never carries that value into FX's
`Usage.cache_write_tokens`; it always stays unknown. Find where usage is parsed and passed along, and make a reported
value reach `cache_write_tokens` end to end. A value the provider did not report must stay unknown (None), never 0, and a
reported 0 must stay 0. Do not change how input, output, cache-read or reasoning tokens are reported, and do not change
the OpenAI-compatible provider. Add tests that show the new behaviour, following the existing test style. Do not edit or
remove existing tests. The project's tests must pass."

Acceptance criteria:
 AC1 reported cache_creation_input_tokens N -> Usage.cache_write_tokens == Some(N) through Anthropic provider + FX executor
 AC2 not reported -> None (not Some(0))
 AC3 reported 0 -> Some(0)
 AC4 existing tests unchanged and passing in fx-single-attempt and fx-native-provider (no test removed or weakened)
 AC5 no behaviour change for input/output/cache_read/reasoning tokens or the OpenAI-compatible path
 AC6 the agent added at least one test covering AC1
 AC7 cargo fmt --check and cargo clippy (both crates, all targets) add no new warnings
 AC8 no unrelated files changed
Independent oracle: hidden test zz_acceptance_cache_write.rs (AC1-AC3, public API only), written before the run.
Baseline: HEAD dd55ac1 in a detached worktree of 23cbdeb; AC1 and AC3 fail, AC2 passes; all existing tests pass (1 environmental TLS test ignored).
Run config: provider openai-compatible, endpoint http://127.0.0.1:6600/v1/chat/completions, model Qwen3.6-35B-A3B-4bit,
 --provider-profile local --structured-output object, --max-turns 40 --max-executions 30 --deadline-secs 900
 --call-timeout-secs 120 --max-output-tokens 8192, state in a scratch FeltDB file. Key via CHIP_API_KEY env only.
