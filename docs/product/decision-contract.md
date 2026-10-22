# The agent decision contract and provider profiles

What a model may say, how the provider is asked to say it, and how each provider's request is written. One contract,
one validator, explicit per-provider request choices.

## The contract (`chip.work-decision.v1`)

A reply is exactly one JSON object (one surrounding code fence and whitespace are tolerated; nothing else is):

| `decision` | required | optional | nothing else |
| --- | --- | --- | --- |
| `request_capability` | `capability` (string) | `inputs` (object, at most 16 entries, values string, integer or boolean) | no other field |
| `complete` | `summary` (string, at most 1,024 bytes of UTF-8) | | no other field |
| `escalate` | `reason` (string, at most 1,024 bytes) | | no other field |
| `block` | `reason` (string, at most 1,024 bytes) | | no other field |

Any decision may carry `"schema":"chip.work-decision.v1"`. The whole reply is at most 4,096 bytes. An unknown decision, a
missing or extra field, a wrong type, a second object, prose before or after the object, a partial object, an undeclared
or unavailable capability, and an oversized value are each an error. There is no repair, no retry and no inferred
default: a refused reply executes nothing. `ModelDecisionBoundary::interpret` is the only reader of a reply, and it
stays authoritative whatever a provider enforced.

## The schema is derived from the contract

`DECISION_SHAPES` (`chip-core`, next to the parser) lists each decision and exactly the fields it may carry.
`ModelDecisionBoundary::response_schema` generates the provider-facing JSON schema from it. A test checks the two against
each other in both directions on a corpus of valid and invalid replies: the schema never excludes a decision the parser
accepts, and the full schema excludes every structural fault in the corpus.

Providers do not all accept the same schema features, so one contract has two projections, chosen explicitly:

| Mode (`--structured-output`) | What is sent | Used for |
| --- | --- | --- |
| `schema` (default) | the full schema: one `anyOf` alternative per decision, so `decision` is required and each decision requires its own fields and allows no others | servers that enforce it (a local grammar does) |
| `schema-flat` | the same contract as one object: `decision` required, an enum of the four values, every field typed, `additionalProperties:false`, each field's description saying which decision it belongs to | providers that reject `anyOf`/`enum`/`const` at the schema root (OpenAI does) |
| `schema-closed` | the full contract (one alternative per decision) restated in the JSON Schema subset Anthropic enforces: every object closed (`additionalProperties:false`), no `maxLength`/`maxProperties`, no `type` beside the root `anyOf`, `inputs` closed over the input names the capabilities declare (`content`, `count`, `length`, `offset`, `path`, `query`, `scope`) | Anthropic (`output_config.format`); opt-in |
| `object` | `{"type":"object"}`: any object | the earlier behaviour, kept to compare against |
| `none` | no `response_format` | providers that cannot carry it |

What a projection cannot say stays with the runtime. The flat schema cannot say that `complete` needs `summary`; JSON
schema `maxLength` counts characters, not bytes, so the byte limit is only approximated; a capability's declared inputs
are known only at run time. Anthropic's structured outputs (`output_config.format`, generally available, no beta header)
require `additionalProperties:false` on every object. Measured against the live API (`claude-haiku-5-5`, 2026-10-20): the
`schema` form is rejected (400, "For 'anyOf', 'type' is not supported"), `schema-flat` is rejected (400, its open `inputs`
object: "'additionalProperties: object' is not supported"), `object` is rejected (400, "'additionalProperties' must be
explicitly set to false"). `schema-closed` is accepted: the per-decision `anyOf` with no `type` beside it, `inputs` closed
over the declared names (each value a string, integer or boolean). A flat closed variant was tried first and is
**not** used: it is accepted but cannot say which decision needs which field, and the constrained decoder filled optional
fields arbitrarily (a stray `summary` on a capability request, no `capability`), which Chip's parser rightly refused. The name list is pinned to the real
capability declarations by a test, so it cannot drift. What it cannot say stays with the runtime: the text-length bound
(the documentation lists `maxLength` as unsupported; one live probe accepted it, which is not relied on), which
capability an input belongs to, and every other rule. Chip parses and validates every reply strictly whatever the provider
enforced; a refusal (`stop_reason: refusal`, mapped to a content-filter finish) or a reply cut at `max_tokens` (a
length-limited finish) is a typed failure and is never a decision. **Measured effect on the work (live, 2026-10-20, synthetic 4-file fixture, one run each, n is small):** with `schema-closed`
enforced, `claude-haiku-5-5` stopped on its first turn in 5 of 5 runs (3 inspect, 2 change) and `claude-sonnet-5-5` in 3 of
3 (2 inspect, 1 change): `block`, `escalate`, or a `complete` that Chip refused ("no authoritative observation satisfies the
goal"). With `--structured-output none`, `claude-haiku-5-5` completed 5 of 5 on the same tasks (3 inspect grounded, 2 change
verified). No reply was an invalid decision, nothing was executed from a refused decision, and no false completion was
accepted: the boundary held, the model's judgment got worse. The cause is **unresolved** (output tokens were similar, 126 vs
127 on the first call; the prompt was not changed to find out). So `schema-closed` is carried correctly and enforced by the
provider, and it must not be made a default or a profile setting on this evidence.
The `anthropic` profile still defaults to `none`; the
Anthropic provider accepts only `none` or `schema-closed`, and any other explicitly requested mode is refused rather than
quietly dropped.

## What the model is told

Every work kind sends the same reply rules (the "question" text): one JSON object matching the contract; every required
field, never leaving out `decision`; no Markdown fence; no commentary before or after; never a partial object; a `summary`
or `reason` of at most 1,024 **bytes** of UTF-8 (not words or tokens), longer making the whole reply invalid; only the
listed capabilities in their exact form, with inputs only where declared; do not say an operation succeeded unless its
result is among the observations shown, and do not treat your own statements as evidence; a reply that breaks a rule is
rejected, not repaired. Instructions never replace validation.

An inspection answers the goal it was given. It states what the observations show (file, observation number, returned
fact) apart from the conclusions drawn, says what the evidence does not establish, recommends only if the goal asks for a
recommendation, and stays within the byte limit without dropping facts to fit. Grounding, coverage and the completion
checks are unchanged. Change and verify keep their own completion rules.

## When a reply is rejected

Every reply is parsed strictly, and only a decision that passes the parser and the capability checks reaches execution.
What happens to a reply that does not depends on why it was refused:

| The reply | Error | What the work does |
| --- | --- | --- |
| Not exactly one JSON object: text before or after it (narration), a second object or array after it, malformed or truncated JSON, an unclosed code fence, or no object at all (a bare array, string, number or `null`) | `DecisionError::MalformedReply` | **Recovers, within a bound**: see below |
| One well-formed object that is not an allowed decision: a missing, unknown, mistyped or over-long field; an unknown `decision`; an unsupported `schema`; a value outside the accepted JSON subset (a nested object, an array or `null` as a value, a float, a number out of range, an unpaired surrogate); a duplicate key | `DecisionError::InvalidDecision` | ends the work `Failed` at once, as before |
| Larger than the reply limit | `DecisionError::InvalidDecision` | ends the work `Failed` at once |
| A request for a capability that is unknown, unavailable or not a valid id | `DecisionError::Capability` | ends the work `Failed` at once |
| A provider failure, a call timeout, or a reply cut at the output cap | an FX failure (`model escalation failed: ...`), not a decision error | ends the work `Failed` at once, with its delivery; never retried |

A malformed reply is never repaired. No object is extracted from the surrounding text, and when there are several, none is
chosen. Recovery runs inside the work loop that already exists:

- **What is recorded.** The turn the reply arrived in ends with no decision. `WorkEvent::DecisionRejected` records the
  turn, the diagnostic, how many malformed replies have arrived in a row, and the limit. Nothing in the reply is
  executed, recorded as an observation, or counted as a write, and it cannot satisfy a goal. A write conflict it might
  have resolved stays unresolved. A response recorded for resume is not given back after a rejection
  (`chip-agent-state` offers a recorded response only when nothing but a decision followed it).
- **What the model is told.** At its next escalation, the model receives the runtime's diagnostic as the first evidence
  item: what was wrong, that nothing was done, and how many more malformed replies the work will take. The diagnostic
  gives positions and counts, never the reply's text. The one exception is a duplicated field name, cut to 40
  characters.
- **What it costs.** Each rejected reply uses a turn and a model call. The turn, execution and context limits, the
  whole-work deadline, cancellation and the stuck guard apply to the following turns exactly as to any others. Nothing
  is reset.
- **The bound.** `WorkSpec::malformed_reply_limit` sets how many malformed replies **in a row** the work recovers from.
  It defaults to `DEFAULT_MALFORMED_REPLY_LIMIT`, which is **2**. The next malformed reply in a row ends the work
  `Failed` with the reason `the model's response is not a valid decision: malformed reply: ... (3 malformed replies in
  a row; the limit is 2)`. A decision that passes validation resets the count. A limit of `0` restores the strict
  behaviour, ending the work at the first malformed reply.
- **How it is reported.** `stack-agent work` prints `rejected  malformed model reply (n in a row, limit 2)` as it
  happens. The report adds a `Malformed replies:` line, and `--json` adds `malformed_replies`, the number rejected.

This recovers from the reply's form only. It does not change how whole-file writes are sent, how PAX reports test runs,
or which tests fail at a project's baseline.

## Provider profiles

A profile is a named, inspectable set of request-compatibility defaults. It describes only the token-limit field, whether
a temperature is sent, and the structured-output mode. It changes no schema, authorization, goal, coverage rule or way of
judging success. Choose one with `--provider-profile NAME` or `CHIP_PROVIDER_PROFILE`; nothing is inferred from a model
name, an endpoint or an error, and a failed call is never retried under another profile.

| Profile | Wire | Token-limit field | Temperature | Structured output | Measured reason |
| --- | --- | --- | --- | --- | --- |
| `openai` | OpenAI-compatible | `max_completion_tokens` | omitted | `schema-flat` | `gpt-5.5` rejects `max_tokens`, any temperature but its default, and `anyOf` at the schema root |
| `anthropic` | Anthropic | `max_tokens` | omitted | none | `claude-sonnet-5-5` rejects `temperature`; only `schema-closed` can be sent (opt-in), so the default stays `none` |
| `local` | OpenAI-compatible | `max_tokens` | 0.0 | `schema` | an OpenAI-compatible server that knows `max_tokens` and enforces the full schema (LM Studio) |

**Precedence**, per setting: its flag, then its environment variable, then the profile, then the default. So
`--provider-profile openai --temperature 0.5` sends 0.5 and the profile's other choices. `work --json` reports
`generation.provider_profile` and, for each setting, its value and source (`flag:…`, `environment:…`, `profile:openai`,
`default`, or `provider` when the provider cannot carry the setting). A profile written for another wire, an unknown
profile, and an explicit structured-output mode on the Anthropic provider are refused before any request.

## Measured effect

See "After the decision contract" in `docs/product/agent-bench.md`. In short: the prompt and answer-contract changes took
Anthropic from 8/15 to 13/15 on the benchmark; OpenAI stayed at 15/15; and the canonical schema, which is not sent to
Anthropic, made the local model worse (6/15 to 3/15 on LM Studio), because under its grammar it answers `complete` before
observing anything. The permissive `object` mode remains available (`--structured-output object`) and is the measured
better choice for that model. Zero unauthorized executions in 60 trials.
