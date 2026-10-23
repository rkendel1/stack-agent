# Held-out fixture verification

Each fixture was checked with `cargo test --offline` (CARGO_TARGET_DIR shared) in its buggy state, then in a temporary copy after `git apply solution.patch`.

**Overall: all 12 fixtures verified**

## f01 — deep

- Failing tests before fix: largest_tier_discount_with_tax, middle_tier_discount
- Before (exit 101):
    - `test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f02 — deep

- Failing tests before fix: escape_sequences_in_strings, escaped_value_in_list
- Before (exit 101):
    - `test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f03 — deep

- Failing tests before fix: last_partial_page, create_then_list_default_page
- Before (exit 101):
    - `test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f04 — dispatch

- Failing tests before fix: long_sms_fits_in_one_segment
- Before (exit 101):
    - `test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f05 — dispatch

- Failing tests before fix: rle_handles_runs_longer_than_255, rle_codec_from_registry_decodes_its_own_output
- Before (exit 101):
    - `test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f06 — dispatch

- Failing tests before fix: burst_probe_on_both_strategies, token_bucket_burst_is_capped_after_idle
- Before (exit 101):
    - `test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f07 — multi

- Failing tests before fix: parses_negative_amounts, account_balances, statement_includes_both_end_dates
- Before (exit 101):
    - `test result: FAILED. 3 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f08 — multi

- Failing tests before fix: parses_prerelease_tag, requirement_naming_prerelease, prerelease_sorts_before_release
- Before (exit 101):
    - `test result: FAILED. 3 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f09 — multi

- Failing tests before fix: snake_case_column_names, mixed_numeric_comparisons
- Before (exit 101):
    - `test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f10 — declaration

- Failing tests before fix: (compile error: acceptance test file does not build; all its tests fail: card_checkout, bank_transfer_fee_is_capped, gift_card_with_alphanumeric_code, malformed_gift_cards_are_rejected, invalid_card_rejected)
- Before (exit 101):
    - `error[E0277]: the trait bound `u32: From<&str>` is not satisfied`
    - `error: could not compile `checkout` (test "checkout") due to 2 previous errors`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f11 — declaration

- Failing tests before fix: large_transfers_are_counted_exactly, top_hosts_by_volume
- Before (exit 101):
    - `test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes

## f12 — declaration

- Failing tests before fix: (compile error: acceptance test file does not build; all its tests fail: parses_compound_durations, duration_errors_explain_the_problem, schedule_reports_why_an_interval_is_bad, runs_are_interleaved, rejects_malformed_jobs)
- Before (exit 101):
    - `error[E0308]: mismatched types`
    - `error: could not compile `cronlite` (test "schedule") due to 9 previous errors`
- After applying solution.patch (exit 0):
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
    - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- Verified: yes
