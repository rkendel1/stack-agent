# cli2: discovering and running the project's own tasks

`pax.test` runs a project's tests. It does not know that the project also has a `lint`, a `build`, a
`fmt` or a `docs` task, and Chip has no way to run one. [cli2](https://github.com/rkendel1/cli2)
does: it discovers a project's tasks (Make targets, npm scripts, cargo and just tasks), plans a run,
asks for approval **of that exact plan** when its policy says to, executes it, and records what
happened. This integration lets Chip use that, without giving up anything Chip already guarantees.

It is opt-in. Start a `cli2 serve`, then:

```sh
cli2 serve --socket /path/to/private-dir/cli2.sock &
stack-agent work --cli2-socket /path/to/private-dir/cli2.sock --cli2-approval stdin "<goal>"
# or CHIP_CLI2_SOCKET=... ; without --cli2-approval nothing that needs approval runs
```

## Who owns what

| | owner |
|---|---|
| the agent loop, model boundary, work state, limits, **completion** | Chip |
| discovery of project tasks, planning, approval policy, execution, the record of what ran | cli2 |
| judging whether the goal is met (`pax.test` passed after the last change) | PAX, decided by Chip |
| the person at the prompt | the host (`Approver`) |

The integration adds no loop, no model call, no store, and no way for a model to approve anything.
Chip does not link cli2: it speaks cli2's versioned local protocol (version 1) over the socket you
give it, so the two are built and released separately. `crates/chip-cli2/tests/architecture.rs`
pins this (dependencies, no persistence, no use of cli2's own task controller).

## What the model gets

Two capabilities, routed by Chip's `CapabilitySet` by declared id like any other:

* **`cli2.tasks`** (an observation): the tasks cli2 discovered, by id, with what each is for and
  cli2's own account of its effects.
* **`cli2.run`** with one input, `task`, a discovered id such as `project-task:make:test`. Never a
  command line. An id cli2 did not discover is refused before anything is planned, and if cli2
  cannot be asked the request fails closed.

**File reads and edits are deliberately not offered here.** Chip already has `project.read` and
`project.write`, with a precondition on the file's hash, stale-base refusal, confinement, protected
paths and bounded size, and its completion rules, test-tamper check and conflict tracking key off
those observations. A second write path would be a way around them. Where the two overlap, Chip's own
capability is used.

## How a run is authorized and executed

1. cli2 **plans** the task (pinned to the id), reporting its effects and whether it needs approval.
2. If it does, the host's `Approver` is shown the whole plan: task, commands, directory, effects,
   why approval is needed, and the **full digest**. The default approver (`DenyAll`) declines. With
   `--cli2-approval stdin` the plan is printed on stderr and the person approves only by typing the
   digest shown; anything else, or end of input, declines.
3. On approval the backend asks cli2 to approve **that digest** (cli2 checks it against the stored
   plan), then starts the execution cli2 approved. A declined plan is retired in cli2 so it can
   never be approved and run later. If cli2's policy forbids the task, nobody is asked.
4. The backend waits for cli2 to record an outcome and hands Chip an observation.

The model's output never reaches steps 2 or 3. The only thing it supplies is a task id.

## What a result means

The first line of a `cli2.run` observation is one canonical JSON object; `outcome` is exactly one of:

| `outcome` | meaning | Chip status |
|---|---|---|
| `verified` | ran, exited 0, and the checks the host declared for this task passed (cli2 evaluated them) | success |
| `succeeded_unverified` | ran and exited 0; nothing task-level was established | success |
| `process_failed` | ran and failed: exit status, signal or timeout | failure |
| `verification_failed` | ran to completion, but a declared check did not hold | failure |
| `rejected` | **nothing ran**: approval declined, policy forbids, or the plan went stale | failure |
| `cancelled` | the run was cancelled | cancelled |

The rest of the output is the task's own output (a bounded tail, plus cli2's `evidence:` references
to the full record): data, never read for meaning. The observation carries references into cli2's
journal, not a copy of its evidence; there is no second store.

**An execution whose result cannot be established is not an outcome.** If cli2 dies while a task
runs, a reply to `start` is lost, cli2 reports an interrupted execution, or it reports a status this
adapter does not know, the backend returns Chip's `OutcomeUnknown`. Chip's durable state keeps that
intent unresolved: the work ends as failed with "outcome unknown", not as a failed command, and a
later attempt cannot treat it as one.

## Completion is still Chip's, and still PAX's

A `cli2.run` result, even `verified`, **does not complete a goal**. Completion remains "`pax.test`
passed after the last change Chip saw", and the model's words are not evidence.

What cli2 results do change is what counts as *after the last change*. A task that ran and that cli2
does not have declared read-only may have altered the project, so it keeps an earlier test pass from
counting (`may_change` in the observation, from cli2's own effects data, never from the task's name).
But it is only a *possible* change: it can invalidate a pass, and it cannot stand in for the observed
write that proves the goal's change was made. A refusal (nothing ran) changes nothing.

## Repeats and recovery

Chip's durable state gives each logical action a key. `cli2.run` declares itself
`Deduplicated`: the key becomes cli2's plan idempotency key, and before approving or starting
anything the backend asks cli2 whether that plan already has an execution. If it does, that execution's
recorded outcome is what comes back (or "unknown" if it was interrupted): never a second run, and no
second approval is asked for. This holds under Chip's one-writer project lock; it is not a
cross-process lock.

## Requirements and limits

* A cli2 that supports protocol version 1 and `capabilities.list` with `kinds` (the unfiltered list
  includes every executable on `PATH` and can exceed a protocol message). An older cli2 is refused
  with a message that says so.
* The project cli2 acts on is the directory Chip works in.
* `--cli2-approval stdin` reads the answer from standard input; a fully unattended run uses the
  default and so cannot run anything that needs approval.
* Host-declared acceptance checks per task (`Cli2Config::expecting`) are available to library users;
  there is no flag for them.
* Inspections are narrowed to read-only capabilities and never see these.

## Testing

`cargo test -p chip-cli2` needs nothing: a scripted cli2 on a real Unix socket. The end-to-end tests
in `crates/stack-agent/tests/cli2_integration.rs` run the real `stack-agent` binary with a scripted
model (no inference service) against a real `cli2 serve`, with a stand-in `pax` that judges the
project's actual files, and the test plays the person at the prompt. They need a cli2 binary
(`CLI2_BIN`, or `../cli2/target/debug/cli2`); without one they say so and stop, unless
`CLI2_REQUIRED=1`.
