# Interruption, recovery, cancellation (items 20-24)

## Cancel in-flight work (22) and process groups (23)

- `work --abort-in-flight` (opt-in): when the work is told to stop (Ctrl-C, `--deadline-secs`, a token
  budget) while a tool is running, the execution is abandoned at once instead of waited for (up to the
  300 s test limit). The outcome is `cancelled`, the reason names the capability and says whether it took
  effect is **unknown**, and a durable task keeps that intent unresolved so a resume refuses to guess
  (settle it, below). Without the flag nothing changes: an execution in flight finishes.
- `pax.test` now runs as its own process group (unix). At the time limit, or when the run is abandoned,
  the whole group is killed (SIGKILL), so a killed wrapper cannot leave `cargo` or a test binary holding
  the project lock. Declared host capabilities (`exec` bindings) already did this. `git` and
  `project.observe` children are single processes and are still killed by pid.
- A model call in flight is abandoned client-side whether or not the flag is set. A remote server may keep
  generating: the report's `remote_cancellation` says whether its stop was confirmed. Chip cannot make a
  remote server stop.
- Tested: `tests/abort_in_flight.rs` with a PAX shim whose run starts a 60 s grandchild: with the flag the
  work ends in under 5 s and the grandchild is dead; without it the work waits for the limit, as before.

## Settle unknown outcomes (21)

The operation already existed as `chip_agent_state::settle` and `stack-agent agent-state settle`. It is now
also an HTTP route on `serve`: `POST /v1/work/{id}/settle` with
`{"intent": KEY, "executed": bool, "decided_by": ID, "evidence": TEXT}`.

- It performs nothing and makes up no receipt: it records an operator's conclusion (a `chip.Observation`
  marked as a settlement, plus a checkpoint by `decided_by`) and reconciles the intent citing it.
- Refused with 409 `settlement_refused`, writing nothing, when the task is held by a worker, the intent was
  observed or is resolved already, the task is finished, or the body is incomplete (400). 501 without
  durable state.
- The route is part of the local trusted-client interface (no authentication). It is not available in
  `--tenants` mode, where works are not durable.
- The success path is tested where intents are created (`agent_state` tests); the route's own contract is in
  `tests/serve_settle.rs`.

## Lease validation (24)

`--lease-secs N` that is not longer than the model call timeout is refused up front (exit 2), naming both
numbers and where the timeout came from. A lease is not renewed while a call is in flight; the refusal is
the fix, not a renewal. A lease shorter than a long tool run (the test limit) is not checked.

## Forced takeover (20): not done

Taking over a task whose owner is dead, before its lease expires, needs an operation in FeltDB's agent
transition contract (`acquire` is refused while the lease is unexpired, and the contract has no operator
override; the only other routes are `handoff` operations that the *current owner* must start). That is a
change to the pinned FeltDB contract, not to Stack-Agent, and inventing a workaround here (editing the
lease record directly, or waiting less than the lease) would bypass the fence the contract exists to
provide. Until the contract has such an operation, the supported route is: wait for the lease to expire, or
start with a shorter `--lease-secs` next time.
