# Held-out v5 fixtures: verification record

Procedure for every fixture:

- Buggy: `cargo test --offline` run from the fixture root, with `CARGO_TARGET_DIR=.../v5-author-build/<g>-buggy`.
- Fixed: the fixture was copied to a fresh directory, `git apply solution.patch` was run there, then `cargo test --offline` with `CARGO_TARGET_DIR=.../v5-author-build/<g>-fixed`. The copy was deleted afterwards.
- Partial and alternative fixes were each applied to their own fresh copy and built in their own target directory (`<g>-var-<n>`).
- All build directories were deleted afterwards.

Toolchain: cargo 1.97.0 / rustc 1.97.0, edition 2021, no dependencies.

The "after" lines show every `test result:` line cargo printed: the unit-test and doc-test harnesses report 0 tests, and the integration test file reports the count shown.

## g01 (deep; producer_consumer)
Failing before fix: `last_page_is_partial`, `filtered_listing_fits_one_short_page`, `checked_out_books_drop_from_available_query`. All three panic at `src/pages.rs:51:21` (the slice in `paginate`).
```
test result: FAILED. 4 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 0 passed; 0 failed; ... (unit)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; ... (doc)
```

## g02 (dispatch; factory_or_helper_type)
Failing before fix: `csv_embedded_quotes_are_doubled`, `quoted_csv_header_with_quote_in_column_name`.
```
test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
(plus the two 0-test harness lines)

## g03 (multi; no situation tags)
Failing before fix: `imported_negative_amounts_keep_their_cents`, `final_balance_of_checking`, `balance_includes_entries_on_cutoff_day`, `month_end_report`.
```
test result: FAILED. 3 passed; 4 failed; 0 ignored; 0 measured; 0 filtered out
```
After full fix:
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
Partial-fix checks:
- Only `parse_amount` fixed: `balance_includes_entries_on_cutoff_day` and `month_end_report` still fail (`test result: FAILED. 5 passed; 2 failed`).
- Only `Ledger::balance_as_of` fixed: `imported_negative_amounts_keep_their_cents` and `final_balance_of_checking` still fail (`test result: FAILED. 5 passed; 2 failed`).

## g04 (declaration; uncompiled_test)
Before fix the test file does not compile:
```
error[E0559]: variant `Backoff::Exponential` has no field named `max_ms`
  --> tests/retry.rs:23:81
error: could not compile `jobretry` (test "retry") due to 1 previous error
```
No tests run. The changed functions `parse_backoff` and `Backoff::delay_for` are exercised by `capped_exponential_from_spec`, `runner_waits_capped_delays` and `uncapped_exponential_spec_still_parses`, which contain no compile errors themselves. The only compile error is in `exponential_delays_are_capped`.
After fix:
```
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## g05 (deep; large_search)
About 877 functions in plain source: 30 stage files with 28 chained rule functions each, plus `apply` per stage, `run_all`, the engine and `price`. All are reachable from every quote test through ordinary calls. The bug is in `stage_21::r14`, about 20 calls deep.
Failing before fix: `regional_bulk_order_quote`, `small_regional_bulk_order_quote`.
```
test result: FAILED. 6 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## g06 (dispatch; implicit_dispatch)
Failing before fix: `same_priority_jobs_print_in_submission_order`, `preview_orders_by_priority_then_age`.
```
test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## g07 (multi; implicit_dispatch)
Failing before fix: `has_matches_codes_in_any_case`, `receive_with_lowercase_code_updates_existing_line`, `low_stock_report_flags_items_at_reorder_point`.
```
test result: FAILED. 4 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
```
After full fix:
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
Partial-fix checks:
- Only `Sku::eq` fixed: `low_stock_report_flags_items_at_reorder_point` still fails (`test result: FAILED. 6 passed; 1 failed`).
- Only `StockLine::fmt` fixed: `has_matches_codes_in_any_case` and `receive_with_lowercase_code_updates_existing_line` still fail (`test result: FAILED. 5 passed; 2 failed`).

## g08 (declaration; no situation tags)
Failing before fix: `heavy_pallet_is_priced_in_top_band`, `bulky_box_billed_by_volume`, `overweight_parcel_is_refused`.
```
test result: FAILED. 3 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## g09 (deep; ambiguous)
Failing before fix: `current_line_at_buffer_start`, `current_line_in_the_middle_of_a_line`, `current_line_after_search_on_last_line`, `typing_updates_the_line_under_the_cursor`, `crlf_lines_are_presented_without_carriage_return`.
```
test result: FAILED. 2 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out
```
After the reference fix (in `LineIndex::line_of`: `Ok(i) => i, Err(i) => i - 1`):
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
Alternative fix check: in a fresh copy, `LineIndex::line_of` was left unchanged and only `Document::line_at_offset` was changed, to `let line = self.index.line_of(offset) - 1;`. Result:
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
`line_of` has no other caller, and no test observes it directly, so the test output cannot tell the two fixes apart.

## g10 (dispatch; factory_or_helper_type, implicit_dispatch)
Failing before fix: `empty_rotated_segment_is_skipped`, `per_component_counts_across_rotation`.
```
test result: FAILED. 4 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## g11 (multi; no situation tags; two-crate workspace cidrkit + aclgate)
Failing before fix: `whole_internet_block` and `catch_all_network_rule` (both panic at `cidrkit/src/net.rs:26:5` with a shift overflow), plus `upper_end_of_port_range_is_included`, `first_match_wins` and `single_port_and_any`.
```
test result: FAILED. 3 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out
error: test failed, to rerun pass `-p aclgate --test acl`
```
After full fix:
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
(plus four 0-test harness lines for the two crates)
Partial-fix checks:
- Only `prefix_mask` fixed: `catch_all_network_rule`, `first_match_wins`, `single_port_and_any` and `upper_end_of_port_range_is_included` still fail (`test result: FAILED. 4 passed; 4 failed`).
- Only `PortRange::contains` fixed: `catch_all_network_rule` and `whole_internet_block` still fail (`test result: FAILED. 6 passed; 2 failed`).

## g12 (declaration; no situation tags)
Failing before fix: `partially_shipped_order_reports_partial_status`, `shipped_email_is_sent_once_everything_has_left`.
```
test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```
After fix:
```
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
