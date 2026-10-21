# Stack-Agent Coding Task — AppPort Dependency Resolution Across Capability Versions

Repository: https://github.com/rkendel1/appport (TypeScript, pnpm workspace, vitest). Start from
the commit you were given; do not fetch newer history.

## Goal

Make AppPort's application dependency resolution correct for providers that serve more than one
version of the same capability, as the specification allows.

## User-visible problem

A provider application registers `invoice.create` at both version 1 and version 2. A consumer
declares a requirement for `invoice.create` with version constraint `"2"` (or `"^2"`). Resolution
reports the requirement as unresolved or incompatible, with a message saying the provider's
version 1 does not satisfy the constraint, even though version 2 is offered by that same provider.
Open-ended constraints such as `">=1"` resolve, but not to the version users expect from the
resolver's documented selection rule. A requirement for exactly version 1 works.

## Requirements

1. Read the relevant specification (`spec/appport-1.0.md`, `spec/dependencies-1.md`) and the core
   resolver and registry before changing code.
2. Reproduce the failure against the starting commit with a test that fails there.
3. Fix the responsible layer so that compatibility, provider-pinned resolution, and resolution
   across several candidate providers all agree on which provided version satisfies a requirement.
4. Keep the existing selection rules between providers (including lifecycle preference and the
   `ambiguous` outcome) and existing result shapes.
5. Add regression tests through the public `@appport/core` API.

## Acceptance criteria

- A provider serving several versions of a capability is compatible with a requirement for any
  version it serves, and incompatible when it serves none that satisfies the constraint.
- Exact, range, and provider-pinned requirements resolve to the version they ask for, following the
  resolver's documented selection rule.
- Single-version providers resolve exactly as before.
- The existing test suite still passes (`pnpm test`).
- The new tests fail when the fix is reverted.

## Constraints

- No change to manifest formats, the wire protocol, or public package APIs.
- No unrelated refactors.
- Reproduce against the starting commit first and report any difference from this description.

## Completion report

Give the starting commit, reproduction command and output, root cause, changed files, regression
tests, full test results, and remaining limitations. Do not claim success if tests fail. Do not
commit, push, or open a pull request.
