# Hosted `serve`: tenants, per-request settings, isolation (`--tenants`, `--profiles`)

Off unless `serve --tenants FILE --profiles FILE` is given; without them `serve` is unchanged (local,
loopback, trusted client, a request may name only `goal` and `kind`).

## What the operator writes

`tenants.json` (`chip.tenants.v1`): per tenant, `id`, `token_sha256` (the SHA-256 of the bearer token; the
token is never stored), a `workspace_root` (must exist; roots of different tenants must be disjoint),
the `kinds` it may submit (`inspect` if omitted; never `act`), its `profiles`, and ceilings:
`max_concurrent`, `max_queued`, `max_turns`, `max_executions`, `max_deadline_secs`.

`profiles.json` (`chip.profiles.v1`): named model settings (`provider`, `model`, `endpoint`,
`api_key_env`, optional `capabilities` file, `streaming`, `structured_output`,
`structured_output_fallback`). Each profile is built at startup, so a bad one stops the service before it
accepts anything. A profile's key comes only from the variable it names; ambient provider keys are off.

## What a request may do

`POST /v1/work` with `Authorization: Bearer <token>` (401 otherwise; `/health` stays open). Besides `goal`
and `kind` it may name: `profile` (one of the tenant's; optional if the tenant has one), `project` (a
relative path inside the tenant's workspace; links are resolved and must stay inside), and `max_turns`,
`max_executions`, `deadline_secs` at or below the tenant's ceilings. Anything else (`provider`,
`endpoint`, `api_key`, `env`, an absolute path, `..`) is refused, not ignored.

## Isolation and quotas

- A tenant sees, reads events of and cancels only its own works; another tenant's id answers 404
  `work_not_found`, as an id that never existed does. `/v1/metrics` returns the caller's own counts.
- Per-tenant concurrency and queue limits (429 `tenant_quota`). A tenant at its limit waits without
  holding back others: the queue is FIFO except that a tenant already at its concurrency limit is skipped.
- Each work runs in its own project directory under its tenant's root, in a fresh environment with its
  profile's capabilities and the project lock, and with its profile's runtime.
- Tokens are compared by SHA-256 in constant time across all tenants.

## Not done, stated plainly

- **No per-tenant durable state.** Tenant works are not admitted to FeltDB, not recovered after a
  restart, and not readable from disk (`durable: false`); `--state-db` is refused with `--tenants`. A
  restart forgets them. Per-tenant data directories for FeltDB are future work.
- No TLS. Put a TLS-terminating proxy in front before listening beyond loopback; the bearer token
  crosses the wire in the clear otherwise.
- No rate limiting by request count, no per-tenant token budget, no workspace disk quota, and no
  per-work copy of a template workspace: `project` names an existing directory, and two works on the
  same project serialize on its lock (the second fails to start rather than waiting).
- Tested with a mock model over real TCP (`tests/serve_tenants.rs`), not with concurrent tenants under
  load or a hostile network client.
