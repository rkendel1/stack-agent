# Compact decision prompt: an A/B experiment (experimental, unproven)

**Question.** Does wording the decision contract more briefly, from the same data, make a small local
model (`woodcock-qwen3-4b` served by Woodcock) produce more *valid and correct decisions* in the real
Stack-Agent work loop? Eliminating one formatting failure is not the criterion: a reply that is valid
JSON and a valid decision but names an invented capability, reads the wrong file or completes without
evidence is a **failure**, not a partial success.

**The hypothesis is unproven.** Read `RESULTS.md` for what was measured.

## What the experimental mode is

`CHIP_DECISION_PROMPT=compact` (or `stack-agent work --decision-prompt compact`); the default,
`full`, is byte-identical to what every release sent. Compact changes exactly two pieces of text the
model is told:

1. **The question** (`chip_core::CompactDecisionBoundary::question`). It is written from
   `DECISION_SHAPES`, `MAX_TEXT_BYTES`, `WORK_DECISION_SCHEMA` and the offered capabilities, so it names
   only decisions, fields, capabilities and inputs the validator knows. It states the same rules as the
   full question (one object, no fence or other text, never partial, required fields, the byte limit,
   request only listed capabilities by exact id, inputs only if declared, no `inputs` for a capability
   with none, do not claim success without an observation, rejected not repaired).
2. **The inspection rule's restatement of the reply format** (`INSPECT_REPLY_FORMAT`): the passage
   that repeats the decision shapes, with a hard-coded `project.read` example, which the question already
   states. Every statement about evidence, grounding and the answer is kept.

It changes nothing about what Stack-Agent accepts: `CompactDecisionBoundary::interpret` *is*
`ModelDecisionBoundary::interpret`. Not touched: `COMMON_RULES` (which still names `project.write` in an
inspection that is not offered it), the per-turn state, the rejection feedback, the limits, recovery,
authorization, persistence, the response schema sent to the server.

## Method

`run.py` runs, for each task and trial, `full` then `compact` (interleaved), with the same binary, repository
copy, model, endpoint, environment, limits (the defaults: 12 turns, 8 executions, malformed-reply limit
2) and `CHIP_CALL_TIMEOUT_SECS=180` in both arms. Only `CHIP_DECISION_PROMPT` differs. Each trial gets a
fresh copy of a pristine repository snapshot and a fresh state directory. `proxy.py` logs every request and
reply independently of Stack-Agent; the runtime's own decisions come from its durable event trail.
Nothing is retried, re-run, tuned or dropped after a result is seen. The scoring is fixed in the header of
`run.py` before any compact result was seen.

Tasks (all read-only, `--kind inspect`): T1 the FX native provider (the primary task), T2 `chip-pax`,
T3 `chip-project`. Trials per mode: 5, 3, 3.

Reproduce:

```sh
git archive HEAD | tar -x -C /tmp/task-repo && (cd /tmp/task-repo && git init -q && git add -A && git commit -qm snapshot)
cargo build --release --locked --bin stack-agent
python3 audit/compact-prompt/run.py --bin target/release/stack-agent --repo /tmp/task-repo \
  --upstream http://127.0.0.1:1338 --model woodcock-qwen3-4b --out /tmp/ab
```

## Five different measures

| Measure | Source | Meaning |
|---|---|---|
| syntactic validity | the proxy's recorded reply | the reply, as a whole, is one JSON object |
| contract validity | the runtime's `DecisionMade` / `DecisionRejected` | the strict boundary accepted it |
| decision correctness | an oracle fixed in `run.py` | an accepted request names a capability and inputs appropriate for the task and the evidence so far; a `complete` follows at least one read inside the target |
| execution correctness | the runtime's `Execution*` events and the report | the accepted request ran; reads succeeded |
| task success | completed + grounded + at least two distinct target files cited that were really read | a heuristic: it does **not** check that the answer's claims are true |

## Limits of this evidence

One model, one server, temperature 0, three tasks, 3 to 5 trials. At temperature 0 trials of one task
are expected to repeat, so the number of *independent* observations is closer to the number of tasks than
to the number of trials. No significance is claimed. Token counts are those the server reported. The
oracle is mine and is stated, not validated. Woodcock was not modified and is not endorsed as production
ready by this experiment.
