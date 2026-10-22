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
A decision that does not is **rejected**: nothing in it is executed, completed, repaired or recorded as evidence, and the
model is told why. What counts as a rejected decision:

| The decision | Error | What the work does |
| --- | --- | --- |
| Not exactly one JSON object: text before or after it (narration), a second object or array after it, malformed or truncated JSON, an unclosed code fence, or no object at all (a bare array, string, number or `null`) | `DecisionError::MalformedReply` | **rejected; recovers within the bound** |
| One well-formed object that is not an allowed decision: a missing, unknown, mistyped or over-long field; an unknown `decision`; an unsupported `schema`; a value outside the accepted JSON subset (a nested object, an array or `null` as a value, a float, a number out of range, an unpaired surrogate); a duplicate key; a reply larger than the limit | `DecisionError::InvalidDecision` | **rejected; recovers within the bound** |
| A request for a capability that is unknown, unavailable to this work, or not a valid id | `DecisionError::Capability` | **rejected; recovers within the bound**; the model is told which capabilities the work offers |
| A request whose inputs its capability's declaration refuses (an undeclared, missing or invalid input, `inputs` on a capability that takes none) | `CapabilityError::InvalidInput` | **rejected; recovers within the bound**; nothing executes |
| A `complete` decision that no authoritative observation supports, or whose answer the goal's answer rules refuse | `completion refused: ...` | **rejected; recovers within the bound**; the model is told what the goal still lacks, or which answer rule the answer does not meet |
| A provider failure, a call timeout, or a reply cut at the output cap | an FX failure (`model escalation failed: ...`), not a decision | ends the work `Failed` at once, with its delivery; never retried |
| A capability the provider reports unavailable when the request is validated (no provider, an availability failure) | `CapabilityError::Unavailable` | ends the work `Blocked` at once: not the request's fault |

No rejected decision is ever repaired. No object is extracted from surrounding text, and when there are several, none is
chosen. A model's claim is never taken as completion: a refused `complete` only ever leads to another decision. Recovery
runs inside the work loop that already exists:

- **What is recorded.** The turn the decision arrived in ends with nothing done. `WorkEvent::DecisionRejected` records
  the turn, the reason, how many decisions have been rejected in a row, and the limit. Nothing in the decision is
  executed, recorded as an observation, or counted as a write, and it cannot satisfy a goal. A write conflict it might
  have resolved stays unresolved. A response recorded for resume is not given back after a rejection
  (`chip-agent-state` offers a recorded response only when nothing but a decision followed it).
- **What the model is told.** At its next escalation, the model receives the runtime's diagnostic as the first evidence
  item: what was wrong, that nothing was done, and how many more rejections the work will take. A diagnostic gives
  positions and counts and the runtime's own state (the goal's progress, the declared capability ids, an answer rule's
  own description), never the reply's text. The one exception is a duplicated field name, cut to 40 characters.
- **What it costs.** Each rejected decision uses a turn and a model call. The turn, execution and context limits, the
  whole-work deadline, cancellation and the stuck guard apply to the following turns exactly as to any others. Nothing
  is reset.
- **The bound.** `WorkSpec::malformed_reply_limit` sets how many rejected decisions **in a row** the work recovers
  from, whatever their kind. It defaults to `DEFAULT_MALFORMED_REPLY_LIMIT`, which is **2**. The next rejection in a
  row ends the work with the outcome that decision alone would have ended it with, and the reason says so, for example
  `completion refused: no authoritative observation satisfies the goal (3 rejected decisions in a row; the limit is
  2)`. A decision that is carried out (a request that passes validation) resets the count. A limit of `0` restores the
  strict behaviour: the first rejected decision ends the work, with its own reason.
- **How it is reported.** `stack-agent work` prints each rejection as it happens. The report adds a `Malformed
  replies:` line, and `--json` adds `malformed_replies`; both count every rejected decision (the name is kept for
  compatibility).

This recovers from what a decision says, never from what happened: provider failures, timeouts and truncated replies
end the work as before. It does not change how whole-file writes are sent, how PAX reports test runs, or which tests
fail at a project's baseline.

## What the model is sent, in what order

Each request is the instructions (the goal and the question, which states the reply contract and lists the
capabilities), then one message per observation in the order they were made, then the turn: where the work stands
(state, evidence, prior decisions, what is ruled out). The instructions do not change from one turn of a work to the
next and the observations are only ever appended, so each request begins with the whole of the one before it except
its last message. A provider with a prefix cache (llama.cpp, and the hosted providers' prompt caches) can reuse that
prefix instead of processing it again. Measured with a 3B model on a 4-core CPU (llama.cpp), the share of each
request's input tokens served from the cache went from about 0 (the instructions used to come last) to roughly
55-90%.

When every requirement of the goal is met by Chip's own check of the observations, the turn says so (`goal progress:
every required output has been observed ...`). A successful request that returns exactly what the same request
returned before is noted in the ruled-out list, with how many times, before the stuck guard would stop the work.

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
