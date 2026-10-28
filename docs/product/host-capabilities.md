# Operator-supplied capabilities (`--capabilities`, `--appport`, `--kind act`)

Stack-Agent can be given capabilities by whoever runs it, instead of only the built-in project
capabilities. The authority model does not change: the operator declares, Chip validates and
constructs every execution, the host or child process executes, and only its own observation is
evidence.

## Declaring capabilities

`--capabilities FILE` reads a `chip.host-capabilities.v1` file (unknown fields are refused):

```json
{"schema": "chip.host-capabilities.v1",
 "capabilities": [{
   "id": "notes.add", "description": "Append a note", "effect": "consequential",
   "idempotency": "deduplicated",
   "inputs": [{"name": "text", "description": "the note", "required": true, "type": "string"}],
   "binding": {"type": "exec", "argv": ["sh", "-c", "printf '%s\\n' \"$1\" >> notes.txt", "sh", "${text}"]}}]}
```

- The program and argument layout are fixed by the file. The model supplies only declared inputs
  (`string`, `integer`, `boolean`), and a placeholder `${name}` may only name a required input.
  A capability that declares no inputs rejects even `"inputs": {}`.
- Ids may not begin with `project.`, `pax.` or `appport.`; duplicates are refused.
- The child runs with a cleared environment (PATH, `CHIP_CAPABILITY`, `CHIP_IDEMPOTENCY_KEY` and
  declared `env`), in its own process group, with a timeout and a bounded output. A timeout kills the
  group and the outcome is *unknown*, never success.

## AppPort hosts

`--appport ORIGIN` discovers an AppPort 1 host (`appport/1` single application, or
`AppPort/host-discovery/1` with `--appport-application`, which also needs a reviewed
`--appport-manifest` because that discovery document carries no input schemas).
`--appport-grant SCOPES`, `--appport-authorize NAMES|*`, `--appport-token-env VAR`,
`--appport-timeout-secs N` describe the most recent `--appport`.

- **Discovery is not authorization.** A capability is offered only if the operator named it in
  `--appport-authorize` and its scopes are within `--appport-grant`. Everything else is reported as
  `CapabilityWithheld` (event, and `host_capabilities.withheld` in the JSON report) with the reason.
- No credential is sent for discovery. The token is read from the named variable only and never
  printed or stored. `http` is accepted only for loopback; redirects are refused; no environment proxy.
- Input schemas are mapped only if flat string/integer/boolean; anything else is withheld, not
  approximated. An idempotency key is sent only to a capability whose manifest says it supports one.
- The answer is classified from the host's response alone. A dropped connection or an ambiguous error
  is *unknown*, is not retried, and does not count as success.

## `--kind act`

`act` completes only when the operator-supplied capabilities named by `--require-success NAME`
(repeatable; with none, at least one) have each executed and **succeeded**, as their own observation
says. The model's `complete` is refused otherwise. `--no-builtin-capabilities` removes the built-in
project capabilities (absent, not hidden). `act` without `--capabilities`/`--appport` is a usage error.

## Limits, stated plainly

- The AppPort path is tested against a **mock** host (both discovery shapes, withheld, auth header,
  drop, refusal); it has not been run against a live AppPort host.
- It adds `ureq` to `stack-agent`. `crates/chip-fx/tests/architecture.rs`
  (`no_crate_in_this_workspace_has_an_http_client_of_its_own`) says FX's provider is the only HTTP
  transport; that test already failed at HEAD because of `chip-appport-source`, and this change is a
  second exception to it that the operator should rule on.
- `exec` is real code execution chosen by the operator's file, not by the model. Review the file as
  you would a script.

## AGENTS.md section 22

1. *Hypothesis:* an externally declared capability set can be added without widening what the model may
   make real. 2. *Boundary strengthened:* declared inputs, withheld-with-reason, authorize-by-name.
3. *What can fail:* a bad host answer, a lost connection, a failing child. 4. *Unchanged:* validation,
   evaluation, limits, no hidden fallback or retry. 5. *Measurement:* the `tests/host_capabilities.rs`
   cases (no run without a declared request, no success from failure/unknown). 6. *Falsified by:* any
   run where an undeclared, withheld or failed action counts toward `verified`. 7. *Changes:* runtime
   surface only; no model-capability change.
