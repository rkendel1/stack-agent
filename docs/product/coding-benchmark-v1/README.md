# Coding benchmark v1: four acceptance-tested repository defects

The question this answers:

> Given a real repository at a frozen commit and a user-level description of a problem, can
> Stack-Agent independently find the actual defect, fix it in the right layer, and leave the
> repository passing an acceptance test it never saw?

Four tasks: two Rust tasks in [Compute](https://github.com/rkendel1/compute) and two TypeScript
tasks in [AppPort](https://github.com/rkendel1/appport). Each is a real defect present at the
frozen base commit. Each was reproduced before freezing by an acceptance test written
independently of any fix. Each was checked to pass with a reference fix, without breaking the
repository's existing tests.

**Status: frozen 2026-10-21. Not yet run against Stack-Agent.** No result below is a Stack-Agent
result.

## Layout

| Path | Who sees it |
| --- | --- |
| `tasks/T*.md` | **the agent**: the task brief, and nothing else |
| `evaluator/acceptance/` | evaluator only: the hidden acceptance tests |
| `evaluator/reference/` | evaluator only: one known-good fix per task (proof the test is satisfiable; not the expected answer) |
| `evaluator/prepare-checkout.sh` | makes the isolated checkout a task runs in |
| `evaluator/run-acceptance.sh` | scores a finished run |

This directory lives in the stack-agent repository. The agent must therefore run where it cannot
read this checkout: build `stack-agent` elsewhere, or give the run a filesystem view that holds
only the prepared task checkout.

## The frozen tasks

| Task | Repository @ base | Area | Brief | Acceptance (hidden) |
| --- | --- | --- | --- | --- |
| T1 | compute @ `1383dc4da8b48af2e7c13d3ddf5aee342f20fd42` | Rust: remote target HTTP auth, revocation, control-plane ownership | `tasks/T1-compute-target-auth.md` | `target_auth_acceptance.rs` → `crates/compute-provider/tests/` |
| T2 | appport @ `268c9b46a71a5f2b4aea86d6c73a4a4371958f1f` | TS: event subscription authorization (connection + HTTP SSE) | `tasks/T2-appport-event-observation.md` | `event-observation-acceptance.test.ts` → `spec/test/` |
| T3 | compute @ `1383dc4da8b48af2e7c13d3ddf5aee342f20fd42` | Rust: execution-policy composition and admission | `tasks/T3-compute-policy-versions.md` | `policy_version_acceptance.rs` → `crates/compute-policy/tests/` |
| T4 | appport @ `268c9b46a71a5f2b4aea86d6c73a4a4371958f1f` | TS: dependency resolution across capability versions | `tasks/T4-appport-resolver-versions.md` | `resolver-multi-version-acceptance.test.ts` → `spec/test/` |

The rest of this section is evaluator-only. It names each defect, which the agent must find for
itself.

### T1: Compute remote target authorization (medium)

The documented contract is in `docs/remote-execution.md`, "Target credentials":

- every request needs a credential, health included;
- revocation takes effect at the next request;
- the trust file "is re-read whenever it changes".

Two real defects violate it at the base:

1. **Revocation can be missed.** `TargetAuthorizer` (`crates/compute-provider/src/credentials.rs`)
   caches the trust file keyed only on its modification time. A rewrite inside one timestamp tick
   is never seen, with these effects:
   - a revoked or deleted credential keeps working (200 instead of 401);
   - a rotated credential is refused;
   - a newly issued one is refused.

   The repository's own tests sidestep this: both `bump()` in `tests/sessions.rs` and
   `set_modified(now + 1s)` in the unit test push the timestamp forward first.
2. **Unauthenticated callers can probe what a target offers.** `RemoteService::respond`
   (`crates/compute-provider/src/lib.rs`) checks whether the endpoint offers jobs or sessions
   *before* it authenticates the caller. So on a target that does not offer them, a request with
   no credential, or a bad one, gets `400 capability_mismatch` instead of `401 unauthorized`.

What the acceptance test checks, all through raw HTTP to `serve_listener`:

| Test | What it checks |
| --- | --- |
| A1 | every protected endpoint × six bad credentials, on a full target and on a run-only target |
| A2 | revocation and deletion within one tick |
| A3 | sessions belong to the control plane:<br>• across rotation;<br>• foreign sessions are indistinguishable from missing ones (`404 unknown_session`, no owner leaked);<br>• the replaced credential is refused |
| A4 | a credential issued within one tick is admitted |

At the base all four fail.

Cross-control-plane isolation of sessions already holds at the base. It is asserted so that a fix
cannot regress it.

**Note: cross-control-plane job reads.** A foreign control plane reading a job gets
`401 "job belongs to another principal"`, while a missing job gets `404`. That difference reveals
whether a job exists, but existing tests assert the 401 deliberately, so the acceptance test does
not score it. An agent that changes it is not penalized for that alone, provided the regression
suite still passes.

### T2: AppPort wildcard subscriptions leak protected events (medium)

The rule is in `spec/appport-1.0.md`:

- §9.4 allows `event.name: "*"`, meaning "every event the session may observe";
- §21.7 says "an event a session may not observe is not delivered to it".

`AppPortConnection.#subscribe` (`packages/server/src/connection.ts`) skips the permission check
for `*`, and the listener forwards every event with no per-event check. The HTTP SSE endpoint
(`packages/server/src/http.ts`) subscribes to `*` by default, so an anonymous
`GET /appport/events` streams protected events.

The acceptance test uses its own application with three events: one requiring `documents.read`,
one requiring `audit.read`, and one public. It checks:

- anonymous sessions, sessions without the permission, sessions with one permission each, and
  sessions holding wildcard permissions;
- that named subscriptions keep the same refusals (`UNAUTHORIZED` / `FORBIDDEN`);
- the SSE stream with and without `events=`.

At the base 6 of 8 fail. The 2 passing tests are controls.

### T3: Compute policy composition denies what every source admits (medium)

The contract is the doc comment on `Policy::intersect`: "a contract satisfies the result exactly
when it satisfies both. The result is never less restrictive than either input."
`docs/policy.md` says `allowed_runtime_versions` entries are matched as prefixes contained in the
reported version.

`intersect` (`crates/compute-policy/src/policy.rs`) intersects the two version lists as exact
strings. So `["3"]` ∩ `["3.13"]` = `[]`, and the combined server and caller policy denies Python
3.13.1, which both sources admit.

The acceptance test compares the composition against "every source admits" on six probe versions,
and checks order independence. It covers:

- nested restrictions in both directions;
- multi-entry lists;
- three sources;
- identical, disjoint and unrestricted sources;
- other runtimes left untouched.

At the base 3 of 5 fail. The identical/disjoint and other-runtime tests are controls.

**Ambiguity, deliberately not scored.** Entries are matched by substring ("contain"), so two
entries where neither contains the other (for example `"3"` and `"13"`) cannot be intersected
exactly as a list. The acceptance test only asserts cases where the answer follows unambiguously
from the two quoted statements.

### T4: AppPort resolver sees only the lowest version a provider serves (medium)

The spec allows several versions at once (`spec/appport-1.0.md` §5: "A server MAY support several
versions of one name simultaneously"). Requirements carry version constraints
(`spec/dependencies-1.md`), and the resolver documents its selection rule: "highest version, then
by application ID".

`evaluateCompatibility` and both resolution paths in `packages/core/src/resolver.ts` take
`provides.find(name)`. Because manifests sort by name and then version, that is always the lowest
version. As a result:

- `"2"` / `"^2"` against a provider of v1+v2 is unresolved;
- `">=1"` resolves to v1;
- a provider-pinned v2 requirement fails.

The acceptance test exercises the following, all through the public `@appport/core` API:

- compatibility across constraints;
- exact, range and pinned resolution;
- choosing between providers by the version each would actually serve;
- the `ambiguous` outcome;
- unchanged single-version behaviour.

At the base 6 of 8 fail.

## How the tasks were verified before freezing

Every verification ran in an isolated worktree at the frozen base. Commands and outputs are in
`evaluator/verification.md`.

| Task | Acceptance at base | Acceptance with reference fix | Existing tests with reference fix |
| --- | --- | --- | --- |
| T1 | 0/4 pass | 4/4 | see `evaluator/verification.md` |
| T2 | 2/8 pass (controls) | 8/8 | 1660 passed, 8 skipped (full suite minus audit snapshot) |
| T3 | 2/5 pass (controls) | 5/5 | see `evaluator/verification.md` |
| T4 | 2/8 pass (controls) | 8/8 | 1660 passed, 8 skipped (full suite minus audit snapshot) |

The acceptance tests use only public APIs and the real HTTP path. They do not depend on any
reference fix's internals. For example, T1 passes whether the trust-file cache is keyed on
content, inode, ctime or anything else that observes a rewrite.

## Changes from the candidate task set

- **Compute remote target authorization: kept, and confirmed.** The defects above reproduce at
  `1383dc4`. The brief is the one proposed, unchanged apart from saying not to commit or push.
- **"AppPort packaged CLI generated-server imports": replaced.** That defect is not in AppPort. It
  is in `rkendel1/appboundry`'s release staging, which rewrote import lines inside the CLI's code
  templates. It is also already fixed there, in appboundry `7a22ca0` (see
  `docs/research/agent-runtime-primitives/experiments/stack-agent/appport-live-l1/README.md`,
  "Defects found"). It could only be benchmarked by freezing appboundry at that fix's parent, which
  is a different repository, and the fix is public history. So it was replaced by T2, not reused
  quietly.
- **T3 and T4 are new.** They were found by auditing documented contracts against the code at the
  base commits. Each was then reproduced and checked as described above.

## Running a task

| Setting | Rule |
| --- | --- |
| Starting state | `evaluator/prepare-checkout.sh Tn DIR`: a fresh clone at the frozen base with no remote, no other refs, no reflog, so no later fix is reachable |
| Agent instructions | `tasks/Tn.goal.txt` as the `stack-agent work` goal, and nothing else: not the defect sections of this README, not the acceptance tests, not the reference fixes. `stack-agent work` refuses a goal over 2,000 bytes, so each `Tn.goal.txt` restates `Tn.md` in plain text within that limit. They have the same problem statement, requirements, acceptance criteria and constraints, and add no hints |
| Time limit | 20 minutes of wall-clock time per task |
| Attempts | one per task |
| Agent output | uncommitted changes in the checkout. A commit invalidates the run (`run-acceptance.sh` refuses a moved `HEAD`) |
| Evaluation | `evaluator/run-acceptance.sh Tn DIR OUT`, after the agent has stopped |
| Evidence kept | for each run, under `runs/<date>-Tn/`:<br>• the brief given;<br>• model id and endpoint;<br>• every model reply and tool call (`stack-agent agent-state show --task-id <id>`);<br>• stderr;<br>• the final report;<br>• `agent.diff`;<br>• evaluator logs and `verdict.txt` |

`run-acceptance.sh` does the following:

1. Saves the agent's diff, including new files.
2. Runs the task's regression suite on the agent's tree *without* the acceptance test:
   - T1: `compute-provider`, `compute-environment`, `compute-cli`;
   - T3: `compute-policy`, `compute-placement`, `compute-cli`;
   - T2 and T4: the full vitest suite except the audit snapshot.
3. Copies the hidden acceptance test in, runs it, and removes it.

**The AppPort audit snapshot.** `spec/test/audit.test.ts` compares against `docs/audit.json`,
which lists every test file. Any new test file, the agent's or the evaluator's, makes it drift. Its
result is recorded in `audit-snapshot.log` but not scored.

### Scoring

| Outcome | Meaning |
| --- | --- |
| **Solved** | acceptance passes, regression passes, `HEAD` unchanged |
| **Partial** | acceptance fails, but the agent's diff fixes at least one acceptance test that failed at the base, and regression passes. Recorded with which tests changed state; not counted as solved |
| **Failed** | anything else, including a run that broke the regression suite or exceeded the time limit |

Also record whether Stack-Agent's own report claimed success (`Verified: yes`). A run where it
claimed success but acceptance failed is a **false claim**, and is reported separately from
Failed. A test the agent wrote and passed is not evidence of success.

## Verified reserves (not frozen)

These defects were confirmed at the same base commits. They are small (one or two lines to fix),
which makes them easier than the four tasks above. To use one, it needs its own brief and hidden
test, written and verified the same way first.

- **Compute, `compute-state`, `apply_in_memory`.** A refused batch rolls back the records but not
  the revision counter. So `MemoryState` reports a moved revision after a commit that changed
  nothing, which contradicts `conformance.rs`: "The revision moves on every committed write and on
  nothing else". The file backend is correct.
- **AppPort, `packages/protocol/src/validate.ts`.** Subscribe envelopes with a namespace wildcard
  (`documents.*`) are rejected, although spec §9.4 and `spec/schemas/envelope.schema.json` allow
  them. **Do not pair it with T2 in the same run.** Fixing it alone extends T2's leak to
  `documents.*`.
- **AppPort, `packages/schema/src/from-json-schema.ts`, `deepEqual`.** It treats arrays and plain
  objects with matching keys as equal, so `{ const: [] }` accepts `{}`.

## Limits of this benchmark

- Four tasks, one attempt each: the result describes these four defects, not a success rate.
- Each acceptance test encodes one reading of a documented contract. The places where the contract
  is ambiguous are named above and deliberately left unscored.
- The briefs describe symptoms and constraints, and never the cause or the fix. Even so, a symptom
  sharp enough to be testable narrows the search: T3's brief names the field involved, and T4's
  names the resolver.
