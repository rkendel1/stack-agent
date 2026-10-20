# OpenTelemetry export

A finished work can be exported as one OpenTelemetry trace, mapped from its durable record in FeltDB
(`chip-agent-state`) under the OpenTelemetry GenAI semantic conventions where they apply. The mapping
and its transport are `crates/chip-otel`. stack-agent calls them once the host's verdict is recorded:
from `work`, from each `serve` work, and from `agent-state export`.

Export is off unless the environment asks for it, and it never changes a work's outcome or exit
status. A refused configuration or a failed delivery is reported (the `telemetry` field of the
report and one `note:` line on stderr), the same way learning reports a storage failure.

## Configuration

The standard OpenTelemetry variables, read strictly:

| Variable | Accepted |
| --- | --- |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | `http://` to `127.0.0.1`, `localhost` (which must resolve to loopback) or `[::1]`, used as given. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The same, with `/v1/traces` appended. |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` | Unset or `http/json`. |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TRACES_HEADERS` | Never: set, they refuse the export. |
| `OTEL_TRACES_EXPORTER` | Unset or `otlp`; `none` turns export off. |
| `OTEL_SDK_DISABLED` | `true` turns export off. |
| `OTEL_SERVICE_NAME` | The `service.name` resource attribute (default `stack-agent`). |
| `STACK_AGENT_OTLP_FILE` | Also (or only) append each request to this file as one OTLP/JSON line, created `0600`. |

A collector anywhere other than loopback is reached through a local collector (an OpenTelemetry
Collector on `127.0.0.1:4318` that forwards). stack-agent itself sends nothing off the machine, and
because OTLP headers exist to carry credentials, it sends none: a local collector adds them. The
request is a single HTTP/1.1 POST with `Content-Type: application/json`, a 2 s connect, read and
write timeout, no redirects (any non-2xx answer is a failure) and no retry. No HTTP client crate is
used for it: the workspace keeps those in FX's native provider only
(`crates/chip-fx/tests/architecture.rs`).

## The trace

One task is one trace. Its id is derived from the task id, and every span id from the task, the
attempt and the position, so the same record always gives the same request.

| Span | From | Attributes |
| --- | --- | --- |
| `invoke_agent stack-agent` (one per attempt) | the attempt's first to last event | `gen_ai.operation.name=invoke_agent`, `gen_ai.agent.name`, `gen_ai.agent.id` and `gen_ai.conversation.id` (the task id), `gen_ai.provider.name` (for `anthropic` and `openai`; `stack_agent.provider` always), `gen_ai.request.model`, the usage totals, `stack_agent.outcome`, the host's verdict (`stack_agent.verified`, `accepted`, `goal.satisfied`, `grounded`, `evidence_invalidated` as a flag, `exit_status`), `stack_agent.interrupted` |
| `chat <model>` (client) | `ModelEscalation` to `ModelCalled` | `gen_ai.operation.name=chat`, `gen_ai.usage.input_tokens`, `output_tokens`, `cache_read.input_tokens`, `cache_creation.input_tokens`, `gen_ai.response.model`, `stack_agent.usage.reasoning_tokens`, `stack_agent.usage.cost`, `stack_agent.context_bytes` |
| `execute_tool <capability>` | `ExecutionRequested` to `ExecutionCompleted`/`ExecutionFailed` | `gen_ai.operation.name=execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.call.id` (the execution id), `stack_agent.output_bytes`, `stack_agent.output_sha256`, `stack_agent.observation_record` |
| span events on the root | every other event | decisions (their label only), capability requests, observations, evidence, goal evaluations, context limits and the terminal state |

Every span and span event carries `stack_agent.provenance` (`host_observed`, `model_inferred`,
`runtime_decided`), as the record does. A usage field the provider did not report is absent, never
0. A failure (a failed call, a failed execution, a failed work) has an error status and
`error.type`. An attempt whose record ends before its terminal event (its process was killed) ends
at its last recorded event with `stack_agent.interrupted=true`, and so does a model call or
execution that was still open there.

## What is never exported

The export is built from an allowlist. Every attribute is a count, a flag, an identifier Chip made
(a work id `work_<16 hex>`, an execution id of that work, an observation record of that task), a
SHA-256, a capability id, a decision label (`complete`, `escalate`, `block`, `request <capability>`),
a member of a fixed set (an outcome, an observation kind, a limit, a stop cause, a delivery
certainty), a provider cost that is a plain number, or a model, provider or service name of at most
128 restricted characters. Nothing else is written. So the goal (only its SHA-256 and size), a prompt,
a model reply, a reason (which can quote the model or a provider's error body), a decision's
summary, file content, an execution's output (only its size and SHA-256), a host path and a
credential never are. Behind the validators, any string that looks like a credential (a known key
prefix such as `sk-`, `ghp_`, `AKIA` or a JWT, or a long mixed-case alphanumeric run) is dropped.

Evidence: `cargo test -p chip-otel` (`tests/mapping.rs`, the mapping on records alone;
`tests/leak.rs`, sentinels and credential shapes in every free-text field of the loop's own events
and in hostile records that put one in every field; `tests/export.rs`, a stub loopback collector,
the file sink and every refusal), and through the binary
`cargo test -p stack-agent --test telemetry_export` (`work` to a collector and a file with a
sentinel goal, model reply, file content and provider key; failed, remote and header-carrying
configurations that leave the outcome unchanged; `serve`; `agent-state export`).

## Exporting a recorded work

```sh
stack-agent agent-state export --task-id work_0123456789abcdef --model claude-sonnet-4-5 > trace.json
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  stack-agent agent-state export --task-id work_0123456789abcdef --send
```

`export` prints the request and sends nothing; `--send` delivers it where the variables above say.
The record does not hold the provider or model the work asked for, so `--provider` and `--model`
add them.

## Limits

- Traces only. No metrics or logs are exported, and there is no protobuf or gRPC transport.
- A work is exported once, when it finishes. A work whose process is killed is not exported by
  that process; `agent-state export` exports its record afterwards.
- `--no-state` works have no record, so they are not exported (the report says so).
- Span times are the record's millisecond timestamps.
- The GenAI semantic conventions are still in development upstream; attribute names follow the
  current `gen_ai.*` registry, and anything they do not cover is under `stack_agent.*`.
