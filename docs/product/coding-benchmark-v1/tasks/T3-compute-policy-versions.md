# Stack-Agent Coding Task — Compute Runtime-Version Policy Composition

Repository: https://github.com/rkendel1/compute (Rust). Start from the commit you were given; do
not fetch newer history.

## Goal

Make Compute's effective execution policy admit exactly the runtimes that every applicable policy
source admits, as the policy documentation describes.

## User-visible problem

An operator's server policy allows any Python 3:

```json
{ "version": 1, "allowed_runtime_versions": { "python": ["3"] } }
```

A caller adds a stricter policy that pins Python 3.13:

```json
{ "version": 1, "allowed_runtime_versions": { "python": ["3.13"] } }
```

Each policy on its own admits a job whose runtime reports Python `3.13.1`. When both apply, the
job is denied with `runtime_version_denied`, and the denial lists no allowed versions, even though
the runtime satisfies both restrictions. Operators see the same thing whenever two policy sources
restrict a runtime's version at different levels of detail.

## Requirements

1. Read the policy documentation (`docs/policy.md`) and the policy, admission, and composition code
   before changing anything.
2. Reproduce the denial against the starting commit with a test that fails there.
3. Fix the responsible layer. The combined policy must never be less restrictive than any of its
   sources: a version that any source denies must stay denied.
4. Keep the policy file format, policy identities/serialization semantics, and admission denial
   codes unchanged unless a demonstrated contract defect requires otherwise.
5. Add regression tests that go through policy composition and admission, not only a private
   helper.

## Acceptance criteria

- A runtime version that every source admits is admitted by the combination.
- A runtime version that any source denies is denied by the combination.
- The order in which sources are combined does not change the outcome.
- Restrictions on other runtimes, and policies that do not restrict versions, behave as before.
- Existing tests still pass (`cargo test -p compute-policy`, and the crates that compose policies:
  `compute-placement`, `compute-cli`).
- The new tests fail when the fix is reverted.

## Constraints

- No unrelated refactors; do not change how a single policy's version restriction is matched.
- Do not weaken policy enforcement to make tests pass.
- Reproduce against the starting commit first and report any difference from this description.

## Completion report

Give the starting commit, reproduction command and output, root cause, changed files, regression
tests, full test results, and remaining limitations. Do not claim success if tests fail. Do not
commit, push, or open a pull request.
