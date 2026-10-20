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
