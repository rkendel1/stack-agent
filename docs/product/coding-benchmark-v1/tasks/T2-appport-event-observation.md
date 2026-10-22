# Stack-Agent Coding Task — AppPort Event Observation

Repository: https://github.com/rkendel1/appport (TypeScript, pnpm workspace, vitest). Start from
the commit you were given; do not fetch newer history.

## Goal

Make AppPort's event delivery honour the specification's authorization rules for event
subscriptions on every transport.

## User-visible problem

An application can declare events that require a permission to observe. For example, an event
`documents.changed` that requires `documents.read`. A client that lacks that permission, or that
has no session at all, is correctly refused when it subscribes to that event by name.

Operators report that such clients can still receive those protected events in other ways
of subscribing that the specification also allows. This happens both over a server connection
and over the HTTP event stream (`GET /appport/events`). Clients that do hold the permission must
keep receiving the events they are entitled to.

## Requirements

1. Read the specification's rules for subscriptions and event delivery (`spec/`) and the server
   implementation before changing code.
2. Reproduce the leak against the starting commit with a test that fails there.
3. Fix it in the layer responsible, using AppPort's existing permission model (including its
   wildcard permissions). Do not add a parallel authorization mechanism.
4. Keep the existing behaviour of named subscriptions: the same refusals, error codes, and
   deliveries.
5. Cover both a server connection and the HTTP Server-Sent Events endpoint in regression tests.
6. Do not change the wire protocol, envelope shapes, or public package APIs.

## Acceptance criteria

- A session never receives an event it may not observe, however it subscribed.
- Public events (no authorization declared) still reach every subscriber, including anonymous ones.
- A session holding a permission (directly or through a wildcard permission) still receives the
  matching protected events.
- Named-subscription refusals keep their current error codes.
- The existing test suite still passes (`pnpm test`).
- The new tests fail when the fix is reverted.

## Constraints

- No unrelated protocol or capability redesign; no broad refactor of the server.
- Do not weaken or remove existing tests.
- Do not assume the report is accurate: reproduce it against the starting commit first, and report
  any difference between the report and what you observe.

## Completion report

Give the starting commit, the exact reproduction command and its failure output, the root cause,
changed files, regression tests, full test results, and remaining limitations. Do not claim success
if tests fail. Do not commit, push, or open a pull request.
