# Results: compact decision prompt vs. full, `woodcock-qwen3-4b`, 2026-10-23

**Verdict: the hypothesis is not supported.** Compact wording raised syntactic and contract validity
and cut input tokens by about a third, but it did not produce a single correct, useful decision in
the real work loop: **0 files were read and 0 of 22 trials succeeded, in either mode**. In compact mode
every one of the 11 trials ended `completed` after one directory listing, with an answer built on no
source file. Under the success criterion set for this experiment (more valid *and correct* decisions in
the real control loop, not merely fewer formatting failures) it failed.

Raw data: `results-2026-10-23/` (`results.jsonl`: every trial and reply; `calls.jsonl.gz`: every request
and reply the proxy saw; `meta.json`; `summary.md`). Method and scoring: `README.md` and the header of
`run.py` (fixed before any compact result was seen).

## Setup (identical in both arms)

Binary `stack-agent 0.1.0` built from this tree; model `woodcock-qwen3-4b` at `127.0.0.1:1338`; task
repository = a `git archive` snapshot of the repository HEAD; `--kind inspect`, default limits (12 turns,
8 executions, malformed-reply limit 2), `CHIP_CALL_TIMEOUT_SECS=180`, temperature 0.0, default structured
output (`json_schema`). Only `CHIP_DECISION_PROMPT` differs. Three read-only tasks (T1 `fx-native-provider`,
T2 `chip-pax`, T3 `chip-project`), 5/3/3 trials per mode, interleaved full-then-compact, 22 trials.

## Totals (11 trials per mode)

| | full | compact |
|---|---|---|
| first request: whole / system message (chars) | 9,914 / 7,712 | 8,027 / 5,825 (-19% / -24%) |
| first request: prompt tokens (T1, server-reported) | 2,029 | 1,636 (-19%) |
| model calls | 41 | 33 |
| **syntactic validity** (reply is one JSON object) | 8 / 41 (20%) | 22 / 33 (67%) |
| **contract validity** (runtime accepted it) | 8 / 41 | 22 / 33 |
| rejected by the boundary | 33 | 11 |
| accepted requests, all `project.list "."` | 8 | 11 |
| accepted `complete` | 0 | 11 |
| **decision correctness**: accepted decisions that were appropriate | 8 / 8 | 11 / 22 |
| accepted but **inappropriate** (a `complete` after no read of the target) | 0 | 11 |
| executions completed / **source files read** | 8 / **0** | 11 / **0** |
| terminal state | 11 `failed` (exit 4) | 11 `completed` (exit 1: inspections are never "verified") |
| grounded (Stack-Agent's own check) | 0 | 11 |
| **task success** (completed + grounded + at least two target files cited that were read) | **0 / 11** | **0 / 11** |
| input tokens, all trials | 92,178 | 61,631 (-33%) |
| model latency, all trials | 169.5 s | 118.5 s (-30%) |

By task (valid / calls): T1 full 5/20, compact 10/18; T2 full 3/12, compact 6/9; T3 full 0/9, compact 6/6.

## What the numbers do and do not mean

1. **The extra closing brace was not removed by the shorter prompt.** In compact mode the model still
   emitted the same `...,"inputs":{"path":"Cargo.toml"}}}` (the real capture used as a test fixture) on
   its first attempt to read after the listing, in every T1 and T2 trial, and the boundary rejected it each
   time. 35 of 35 read attempts, in both modes, were for the repository's root `Cargo.toml`, a file that
   is not under any of the three targets. In full mode the first reply of T3 already carried the extra
   brace. The failure is therefore not explained by prompt length at 5.8k characters.
2. **The validity gain is the model abandoning the failing action.** After one or two rejections the
   compact prompt's model answered with `complete` instead of retrying the read. That raises "valid" and
   lowers "rejected" while reading nothing. A valid, accepted `complete` before any source was read is a
   wrong decision, scored as one, not as partial success.
3. **Compact turned a loud failure into a quiet one.** Full mode ends `failed`, exit 4, visibly. Compact
   mode ends `completed`, `grounded`, exit 1, with an answer such as (T2): *"...finds the PAX executable
   by locating the `pax` binary in the project's executable search path. It checks that the binary is
   indeed PAX by verifying its signature or version information... The evidence does not provide direct
   access to the source code..."*. It states mechanisms it never observed, then admits it saw no source.
   Stack-Agent accepted it because the existing inspection rule accepts a completion that cites a path
   observed through a successful operation and says plainly that it "does not evaluate whether a claim is
   true". That rule is not part of this experiment and was not changed; the experiment exposed how weak it
   is when a model completes early.
4. **The one clear gain** is T3's first decision: full mode's valid-listing rate there was 0/3 (the
   extra brace on a `project.list`), compact's was 3/3.
5. **Nondeterminism.** Full mode repeated identically across trials of a task. Compact mode did not
   (3 to 5 distinct reply sequences across 3 to 5 trials, call counts 3 or 4), although temperature was 0.
   Not investigated.

## Sample and caveats

One model, one server, one date, temperature 0, three tasks, 3 to 5 trials each. At temperature 0 the
trials of one task are close to one observation, so the effective sample is nearer 3 than 22. No
significance is claimed, and none is needed for the headline: zero successes in both arms.
The decision oracle and the success rubric are mine, were fixed beforehand, and are unvalidated.
`COMMON_RULES` (which still names `project.write` in an inspection) and the per-turn state text were
deliberately left unchanged, so compact changes the question and one duplicated passage and nothing else;
it does not test a shorter *whole* prompt. Token counts are those the server reported. This experiment
says nothing about whether Woodcock is production-ready.

## What this supports, weakens, and leaves open

- **Weakened:** that prompt length or restating the contract is the cause of the extra brace, or that
  compact wording improves correct decisions for this model.
- **Supported, narrowly:** compact wording is accepted by the unchanged strict boundary, shrinks the
  request, and raises the share of replies that parse, here by making the model stop attempting the read.
- **Open:** why the model emits `}}}` on a nested-object request and why it only ever tries to read
  `Cargo.toml`. Candidates this experiment did not test: Woodcock's decoding path (speculative vs plain),
  how the server applies the `json_schema` constraint to nested objects, and the role of the observation
  message. Those are server/decoding hypotheses and need their own controlled test.

## Recommended next step

Do not adopt compact mode and do not spend more effort on prompt wording for this failure. Run one
controlled test of the decoding path, holding the logged `full`-mode request fixed: the same request
against Woodcock with speculative decoding on and off, and with `response_format` on and off, counting how
often the reply carries the extra brace. Separately, and only as its own reviewed change, consider whether
an inspection should be allowed to complete before any file has been read at all.
