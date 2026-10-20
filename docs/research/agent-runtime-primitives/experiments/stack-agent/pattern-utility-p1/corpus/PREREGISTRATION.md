# Pre-registration

`tasks.json` (goals, selector task representations, expected relevant patterns, known ambiguities,
pattern checks) and `acceptance/` were written **before the selector was run on any of these tasks
and before any model trial**. The only things looked up first were graph symbol ids (to write the
task representations) and the shape counts of the `CapabilityProvider` family already printed in
the P0 report. The expectations are mine, are few, and are stated as the report entries
(category, family, signature) the selector should return, or should not.

Acceptance tests are host-owned and hidden from the model. Reference solutions in `reference/` exist
only to prove each acceptance procedure passes on a correct solution and fails on the unmodified
repository; no model sees them.

## Amendment (before any model trial, after one selector run)

The first version of tasks 2, 3 and 4 asked for edits in `crates/chip-core/src/lib.rs` (46.7 KB) and
`crates/chip-core/src/work.rs` (133 KB). `project.write` replaces a whole file and is limited to 32 KiB,
so **no model could have done those tasks through `stack-agent work`**. I found this by trying to run
a trial, not by design. Tasks 2, 3 and 4 now put the new code in new files of `chip-experiments`, whose
`lib.rs` is 2 KB, with the same pattern families as before. Only their goal texts, crates, selector task
representations and allowed paths changed; the expected relevant patterns and the acceptance
procedures' structure did not. I had seen the selector's output for the first versions (it returned the
expected patterns for all six) when I made the change, so these expectations are not independent of that
run. The corpus validation now also checks that every existing file a task edits is at most 32 KiB.
