# Live event stream (`work --events`)

`stack-agent work ... --events FILE` (or `--events fd:N`, unix, N >= 2) writes each event the loop
records as one JSON line, flushed, while the work runs. `--json` still prints only the final report
on stdout; the durable state stays locked for the run, so this is how a host shows a run live.

```json
{"schema":"chip.work-event.v1","work_id":"...","seq":0,"at_ms":1767225600000,"event":{"kind":"WorkStarted", ...}}
```

- `seq` starts at 0 and has no gaps. A reader that sees one has lost a line.
- `event` is the same rendering the HTTP `/v1/work/{id}/events` route uses: model calls, decisions,
  capability requests, executions, observations, evidence, `GoalEvaluated`, and the one terminal event.
  Model prose is not an event, and the API key never appears.
- The stream observes; it decides nothing. If a write fails the stream stops, one note goes to stderr,
  the final report's `event_stream` says `{"lines": N, "failed": "..."}`, and the work continues.
- An unopenable destination is refused before any model call (exit 3).
- Not done here: OTLP push (telemetry export exists separately; see `telemetry.md`), and SSE/webhook
  push for `serve` (item 27).
