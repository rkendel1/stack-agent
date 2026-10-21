# Stack-Agent Coding Task — Compute Remote Target Authorization

Repository: https://github.com/rkendel1/compute (Rust). Start from the commit you were given; do not
fetch newer history.

## Goal

Ensure the Compute remote-execution target consistently enforces the documented credential,
revocation, and control-plane ownership contract across its HTTP endpoints and durable job
lifecycle.

## User-visible problem

A remote target must not expose protected operations to unauthenticated callers, allow revoked
credentials to remain effective, or expose one control plane's jobs and sessions to another
control plane.

The repository documents target credentials, control-plane ownership, revocation, and remote
job/session operations. Determine whether the implementation actually enforces these guarantees
end to end.

## Requirements

1. Inspect the existing implementation, documentation, and tests before changing code.
2. Reproduce at least one concrete implementation defect or identify a missing regression test for
   a documented behavior.
3. Use the existing Compute authentication and authorization abstractions. Do not add a parallel
   credential system.
4. Ensure all protected remote endpoints enforce the intended authentication contract, including
   health/capability and job/session operations where applicable.
5. Verify that revocation takes effect according to the documented contract.
6. Verify that a credential associated with one control plane cannot inspect, mutate, or operate on
   another control plane's jobs or sessions.
7. Preserve existing error semantics, durable state, execution receipts, and successful authorized
   requests.
8. Add regression tests through the real HTTP/API path, not only helper-level unit tests.

## Acceptance criteria

- Missing, malformed, incorrect, and revoked credentials are refused wherever authentication is
  required.
- A valid credential can perform its authorized operations.
- Cross-control-plane access is refused without leaking protected resource information.
- Credential rotation/revocation behavior matches the documented contract.
- Existing remote execution, job lifecycle, and session tests remain passing.
- Tests prove the behavior before and after the fix.
- No security claim is added to documentation unless supported by implementation and tests.

## Constraints

- No new daemon, sidecar, authorization service, or credential format unless the existing contract
  proves insufficient.
- No unrelated refactors or performance work.
- Do not weaken policy enforcement to make tests pass.
- Do not assume a reported gap is still present: reproduce it against the starting commit first.
- Report the exact defect found, the test that reproduced it, and the evidence that the fix closes
  it.

## Completion report

List the starting commit, reproduction steps, changed files, regression tests, full test results,
and any remaining security limitations. Do not claim success if the acceptance tests do not pass.
Do not commit, push, or open a pull request.
