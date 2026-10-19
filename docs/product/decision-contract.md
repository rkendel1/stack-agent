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
| `object` | `{"type":"object"}`: any object | the earlier behaviour, kept to compare against |
| `none` | no `response_format` | providers that cannot carry it |

What a projection cannot say stays with the runtime. The flat schema cannot say that `complete` needs `summary`; JSON
schema `maxLength` counts characters, not bytes, so the byte limit is only approximated; a capability's declared inputs
are known only at run time. Anthropic's structured outputs (`output_config.format`) require `additionalProperties:false`
on every object and reject `anyOf`, which cannot express a capability request's free-form `inputs`, so the Anthropic
provider carries **no** schema and is validated by Chip alone; an explicitly requested schema mode for it is refused
rather than quietly dropped.

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

## Provider profiles

A profile is a named, inspectable set of request-compatibility defaults. It describes only the token-limit field, whether
a temperature is sent, and the structured-output mode. It changes no schema, authorization, goal, coverage rule or way of
judging success. Choose one with `--provider-profile NAME` or `CHIP_PROVIDER_PROFILE`; nothing is inferred from a model
name, an endpoint or an error, and a failed call is never retried under another profile.

| Profile | Wire | Token-limit field | Temperature | Structured output | Measured reason |
| --- | --- | --- | --- | --- | --- |
| `openai` | OpenAI-compatible | `max_completion_tokens` | omitted | `schema-flat` | `gpt-5.5` rejects `max_tokens`, any temperature but its default, and `anyOf` at the schema root |
| `anthropic` | Anthropic | `max_tokens` | omitted | none | `claude-sonnet-5-5` rejects `temperature`; structured outputs cannot express `inputs` |
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
