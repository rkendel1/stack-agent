# Chip's single-symbol repair loop (experimental)

**Status: experimental, isolated.** The loop lives in `crates/chip-symbol-loop`. Its only entry
point is the `stack-agent` example `symbol_loop`, which `stack-agent` lists as a dev-dependency, so
the product binary never links it. `work`, `verify` and `serve` are unchanged.

> Chip decides what happens next. FX proposes one bounded change. Code enforces the contract. Tests
> establish success.

## 1. Components

| Part | Where | What it owns |
| --- | --- | --- |
| State machine | `machine.rs` | Every transition, as deterministic code. Budgets, termination reasons, recovery |
| Packet and decision contract | `contract.rs` | `chip.symbol-repair.packet.v1` (`TaskPacket`) and `chip.symbol-repair.classify.v1` (`ClassifyPacket`), bounded and serializable. The four decision ops and the one classification op, validated strictly |
| Symbol index | `symbols.rs` | Every function with a body (free functions, impl methods, trait default methods), with exact byte spans, a structural signature, a source fingerprint and the names it calls. Built with `syn` 2 and `proc-macro2` span locations, the parser `chip-graph` uses. Ids follow `chip-graph`'s `symbol:<file>:<path>` form |
| Edit boundary | `edit.rs` | Resolve, verify the fingerprint, parse the body, splice it, reparse, compare the signature, prove the change is minimal, then write only against the exact prepared digests |
| Acceptance | `acceptance.rs` | The task's command (argv fixed by Chip) run with a timeout and a cleared environment. Diagnostics are parsed (compile errors, panics, failed tests). A pass needs exit 0 **and** the required evidence (for Cargo: every `test result:` is `ok`, at least N passed, 0 failed) |
| Targeting | `target.rs` | A ranked, explained candidate list from diagnostics and the index (§9): direct evidence, then the failing test's call chain within its crate and imports, two production levels deep. One target only on direct evidence |
| FX adapter | `fx.rs` | `decide(packet)` and `classify(packet)` over an already-configured `chip-inference` `ModelProvider`. Pure inference: one request, one reply text, the reported usage |
| Checkpoints | `store.rs` | FeltDB (the store `chip-agent-state` uses), through its guarded atomic transactions. One transition is one commit: the run's state document (fenced on its version) plus a create-only event record for that sequence number |

## 2. The loop

```text
Acceptance ──pass + evidence──▶ Done(success)
    │ fail
    ▼
Target ── none ─────────────────▶ Done(no_target)
    │ ambiguous ──▶ ClassifyInFlight (once, among supplied ids) ──▶ Done(invalid / ambiguous) or a target
    │ one symbol
    ▼
Decide (packet persisted) ──▶ DecisionInFlight (intent committed, then one call)
    │ reply validated in Rust
    ├─ invalid, no_change, bad symbol, no_parse, signature moved ──▶ rejection ──▶ new packet
    ├─ need_symbol (an authorized neighbor) ──▶ packet with that source
    ├─ stale source ──▶ Done(stale_source)
    └─ valid body ──▶ Apply (patch intent with before/after digests) ──▶ Acceptance (patch under test)
                                   └─ fails ──▶ rejection ──▶ Revert ──▶ new packet
```

- **Rejections.** Two rejected proposals in one repair attempt end the run
  `repeated_rejection`. Scope is never widened automatically. An attempt is one target symbol; if the
  diagnostics later point elsewhere, a new attempt starts.
- **A rejected body is reverted** before the next packet, so every candidate is applied to the
  original source, and a stopped run leaves the worktree as it found it.
- **A model's reply is never a completion signal.** There is no `done` op. Only the acceptance
  command, with its evidence, ends a run in success.

## 3. Contracts

**`TaskPacket` (v1).** It has these parts, each bounded by `PacketLimits`:

- `goal`: one line, at most 400 bytes;
- `acceptance`: Chip's command, the exit status, and an excerpt of at most 4 KiB;
- `symbol`: id, path, name, lines, fingerprint, and the current source, at most 8 KiB;
- `neighbors`: at most 12 signatures, with no bodies;
- `rejected`: at most 2 bodies with their errors;
- `constraints`: `signature_frozen`, `allowed_ops`, `insertion_after`, `max_body_bytes`,
  `context_requests_left`;
- `context`: at most 2 `need_symbol` sources.

The whole packet is at most 32 KiB, and a packet over any bound is never sent (`packet_too_large`).
The packet's canonical JSON bytes are what is sent and what is digested.

**Decisions:**

| op | fields | Chip's checks |
| --- | --- | --- |
| `replace_symbol_body` | `symbol`, `body` | the symbol is the packet's; the body is one `{ … }` block of at most `max_body_bytes`; then the full edit boundary (§4) |
| `insert_symbol` | `after`, `declaration` | **refused**: never offered, and refused with a typed error even at an authorized point. No insertion rule exists yet |
| `need_symbol` | `symbol` | the id is one of the packet's neighbors and is in Chip's index; context requests are bounded (2) |
| `no_change` | `reason` | recorded as a rejection, since the acceptance command still fails |

The reply must be exactly one JSON object. Surrounding whitespace and one Markdown fence are allowed;
anything else is refused:

- prose, a second object or trailing text;
- duplicate keys or unknown fields;
- wrong types, empty or oversized values;
- unknown ops.

`decision_schema` produces a JSON schema for a server that enforces grammar-constrained decoding.
Whether the local server enforces it was **not verified** (no local server exists in this
environment), so the entry point does not send it and relies on strict validation.

## 4. The edit boundary

For `replace_symbol_body`, each step is code:

1. The file is resolved inside the worktree. A non-relative path, `..`, or a symbolic link anywhere
   on the path is refused.
2. The symbol is re-indexed from the current file. Its fingerprint must equal the packet's
   (`stale_source`).
3. The body is trimmed and must be one `{ … }` block that `syn` parses as a `Block` (`no_parse`).
4. It is spliced into the exact body span Chip resolved, and the file is reparsed (`no_parse`).
5. The symbol must still exist at the same id. The structural signature is compared component by
   component (`signature_moved`), as token streams, so layout and comments don't count:
   attributes (doc comments included), visibility, `const`/`async`/`unsafe`/`extern`, name,
   generics, where clause, parameters and return type.
6. Prefix and suffix outside the body are byte-identical. Every other symbol in the file keeps its
   fingerprint, and the symbol count is unchanged (`bad_symbol`).
7. The patch's identity is `sha256(path, before, after)`. A body-region diff is recorded.
8. `write_checked` writes only if the file still has the prepared `before` digest, or already has
   `after` (idempotent). The write goes to a temporary file, which is then renamed into place.

The model never supplies a path, offset, line, command or search instruction.

## 5. Checkpoints, idempotency and recovery

Everything needed to resume is in the run's FeltDB document (`RunState`):

- phase, sequence number, decision count, accumulated elapsed time;
- target and fingerprint, packet and digest, validated decisions;
- rejected bodies and errors, acceptance summaries;
- the pending, under-test and applied patches, including texts and digests;
- usage and termination.

Each transition also writes an event record (`chip.symbol-loop.event.v1`) with from, to and detail.

| Effect | Protocol | On resume |
| --- | --- | --- |
| Inference call | `DecisionInFlight` / `ClassifyInFlight` committed **before** the call | ends `inference_outcome_unknown`. The call is never reissued blindly |
| Patch write | `Apply` committed with before/after digests and texts, then the write | `write_checked`: already applied → recorded `already_applied`; still `before` → written once; anything else → `stale_source` |
| Revert | `Revert`, the same in reverse | the same |
| Acceptance | an observation | re-run |
| Commit itself | FeltDB atomic transaction (state with `expected_version` plus create-only event) | an identical replay is recognized (`Commit::Replayed`) and writes nothing; a different transition from a stale version is a conflict |

What is **not** atomic: the file write and the commit. The table above is the recoverable protocol
that covers that gap. Elapsed time is measured with a monotonic clock within a process and
accumulated in the checkpoint. Time between a crash and a restart is not counted.

## 6. Budgets and termination

| Budget | Default |
| --- | --- |
| Inference decisions (a classification counts) | 8 |
| Elapsed time (acceptance runs and inference included) | 10 min |
| Rejected proposals per repair attempt | 2 |
| Context requests | 2 |
| Source changes per decision | 1 symbol body (the only edit there is) |
| Packet size | 32 KiB |

Every run ends with a typed reason and a reporting class:

| Reason | Class |
| --- | --- |
| `success` | `pass` |
| `budget_exceeded` | `budget_exceeded` |
| `repeated_rejection` | the last rejection's class: `no_parse`, `bad_symbol`, `signature_moved` or `test_still_fails` |
| `stale_source` | `stale_source` |
| `infrastructure_failure` | `infrastructure_failure` (provider failure, command missing, dirty worktree, unreadable files) |
| `no_target` | `no_target` |
| `ambiguous_target` | `ambiguous_target` |
| `invalid_decision` | the classification reply was invalid |
| `inference_outcome_unknown` | `inference_outcome_unknown` |
| `packet_too_large` | `packet_too_large` |

## 7. Limitations

- **One function body per decision.** Multi-symbol fixes (benchmark T1 has two separate defects)
  are out of scope. Insertion is refused.
- **Functions only.** Structs, enums, constants, `impl` headers, macros and `build.rs` are not
  targets.
- **Targeting is structural.** It finds the function containing an error or panic, or the
  functions a failing test calls by name. A failure whose cause is several calls away from the
  test is resolved to the called function, or to several (ambiguous), not to the cause.
- **Acceptance tests must be in the indexed tree.** An acceptance test copied in and removed
  around the run (as `coding-benchmark-v1`'s `run-acceptance.sh` does) leaves panics pointing at a
  file that is gone, so targeting fails.
- **Rust only.** AppPort tasks (T2, T4) are TypeScript.
- **Grammar decoding is unverified.** See §3.
- **The inference call has no loop-level timeout.** It relies on the provider's own call timeout.

## 8. Evaluation (2026-10-22)

**No inference was spent, and no repair was attempted by a model.** No local inference server is
configured in this environment: no llama.cpp, Ollama or LM Studio process, and no listening model
port. The task forbids hosted inference, new serving infrastructure and model downloads. So the
evaluation ran Chip's stages up to the first inference call, against a closed loopback port, and
stopped there (`infrastructure_failure`) by design.

**Tests (scripted replies, real everything else).** These 12 tests use a real Cargo project, the
real `cargo test` acceptance command, real FeltDB checkpoints and the real edit boundary, and an
independent re-run of the tests and `git diff` after success:
- a valid body is repaired and verified;
- a failing body is rejected, reverted and shown in the next packet;
- two rejections stop the run;
- `no_change`, malformed and unauthorized replies are classified;
- `need_symbol` adds context;
- budgets stop the run;
- infrastructure failures are kept separate;
- restart and replay work, including resuming after the inference intent;
- classification works.

A scripted reply is not a model. Replay candidates for a stronger model: none, because no model
reply exists.

**The frozen tasks:**

| Task | Applicable? | Run | Acceptance | Independent evaluator | Decisions (inference) | Valid bodies | Patch | Termination / class |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| T3 (Compute policy versions, Rust) | yes, with an evaluation-only candidate (below) | 4.0 s | exit 101: 3 failed, 2 passed | not run: no patch to evaluate | 1 recorded intent, 0 answered (connection refused) | 0 | no | `infrastructure_failure` (no local model) |
| T1 (Compute target auth, Rust) | no: two independent defects, so not a single-symbol repair | not run | – | – | – | – | – | out of scope |
| T2, T4 (AppPort, TypeScript) | no: the loop indexes and edits Rust only | not run | – | – | – | – | – | out of scope |

**T3 in detail.** The frozen checkout comes from the evaluator's unchanged `prepare-checkout.sh`
(`1383dc4`); it was not modified. The candidate is a clone of it with one evaluation-only commit
that adds the frozen acceptance test (`crates/compute-policy/tests/policy_version_acceptance.rs`,
`sha256 4e424204…`). The test therefore sits in the indexed tree and the worktree is clean. The
acceptance command is
`cargo test --offline -p compute-policy --test policy_version_acceptance`. An independent score
would apply only the loop's source patch to a fresh frozen checkout and run the unchanged
`run-acceptance.sh T3`.

What Chip did:
1. Observed the failure: 3 failed tests and 3 panics, all at `policy_version_acceptance.rs:89`,
   inside the test helper `assert_exact`.
2. Resolved candidates from the failing tests' calls. The result was **ambiguous**:
   - `compute-cli … policy` (×2), `Daemon::versions`, `ProviderPool::policy`;
   - **`EffectivePolicy::compose`**;
   - `Snapshot::all`, `SnapshotSource::all`, `Query::all`.
3. Started the one classification, which failed at the closed port.

**Finding: Chip's targeting is not yet adequate for T3, independent of any model.**
- The test reaches the code through helpers in its own file (`compose`, `policy`, `versions`,
  `admitted`). Matching their names across the whole workspace pulled in unrelated functions from
  other crates.
- The candidate list is cut to 8 in id order, with no relevance ranking first.
- The defect the reference fix changes, `Policy::intersect`, is **not a candidate**: it is one
  call below `EffectivePolicy::compose`. A perfect classifier could only have chosen `compose`, the
  wrong layer under the task's own rule ("fix the responsible layer").

So T3 is **not supported** by this implementation, and the cause is Chip's targeting. Candidate
fixes for a separately reviewed change:
- follow same-file helper calls transitively;
- restrict candidates to the crates the test imports (`use compute_policy::*`);
- add one level of callee edges from the index (`compose` → `intersect`);
- rank by relevance before truncating.

**Verified completions per minute and decision counts** cannot be compared: there were no model
decisions. With n = 1 applicable task and no inference, nothing here speaks to coding capability.

## 9. Call-graph-aware targeting (follow-up to §8)

§8 showed that Chip could not name the function holding T3's defect. Targeting is rebuilt as a
ranked, explained resolution. Nothing else about the loop changes.

1. **Direct evidence.** A compile error or panic located inside a production function (+100).
2. **Seeds.** The failing tests, matched by name and, when given, by module path. Also any test
   helper a panic is located in.
3. **Scope.** The seeds' own crate and the workspace crates their files `use`. Crates come from the
   nearest `Cargo.toml` `[package]`. An unknown crate with no imports widens the scope to every
   crate, and the ranking says so.
4. **Call chain.** Same-file test helpers are transparent. Production functions the test or its
   helpers call are depth 1, and their callees are depth 2. **An edge needs evidence:**
   - a same-file helper;
   - a path ending in the callee's qualified name (`EffectivePolicy::compose`);
   - a crate-rooted path;
   - a unique or shared name for a free call;
   - for a method call, a method of a type the caller itself names (or its own `Self`).

   A qualified path that names no indexed function (a derived `Default`, `std`, an external
   crate) gives no edge.
5. **Ranking before truncation.** Integer scores, each with its reason. The reasons sum to the score
   (tested).

   | Signal | Points |
   | --- | --- |
   | Depth 1 / depth 2 | +40 / +20 |
   | Qualified path / unique name / one of k shared names | +15 / +10 / +10/k |
   | The test's own crate / an imported crate | +10 / +5 |
   | A method of a type the failing test uses / uses such a type | +5 / +2 |
   | Each identifier with an underscore, 8+ bytes, from a compile-error or panic message that the body uses (at most 2) | +10 |
   | Each further caller on the failing path (at most 3) | +3 |

   Ties are broken by id, and every tie is listed in the notes.
6. **Decision.**
   - One target if exactly one candidate has direct evidence, or there is exactly one candidate.
   - Several direct candidates: ambiguous among them.
   - Otherwise: ambiguous over the top 8 of the ranking, for the one bounded classification or an
     explicit stop. **Proximity never selects a single function.**

   The ranking (seeds, scope, notes, top 8 with reasons) is recorded with the transition it leads
   to.

**T3, re-run** with no inference: the read-only tool `rank_target` against the evaluation candidate
(§8). Ranking in `symbol-loop-runs/2026-10-22-T3-targeting/ranking.json`.

| | Before (§8) | After |
| --- | --- | --- |
| Scope | the whole workspace | `compute_core`, `compute_policy` (the test's crate and imports) |
| Candidates | 8+ by name, cut in id order | 21, ranked |
| `Policy::intersect` | absent | **7th**, offered to the classification |
| Decision | ambiguous (wrong set) | ambiguous (explicit; nothing selected on proximity) |

`intersect` scores 55:
- +20: depth 2, called by `EffectivePolicy::compose`;
- +10: the only `intersect` in scope;
- +10: in the test's own crate;
- +5: a method of `Policy`, which the test uses;
- +10: its body uses `allowed_runtime_versions`, which the panic message names.

Ranked above it: the five functions the test reaches directly (`admit`, `ExecutionContract::from_bundle`,
`EffectivePolicy::compose`, `Policy::from_json`, `WorkloadBundle::create_from`) and `Policy::canonical`.
`canonical` ties with it at 55 and is ordered first by id.

**Caveats, stated plainly:**
- **Two edge rules and one signal were added after looking at T3:** no edge from an unresolved
  qualified path, method edges only to a type the caller names, and the failure-term signal. They
  are general rules, applied uniformly. But T3 is no longer a held-out test of them, and their
  value needs a task they were not designed against.
- **The truncation width (8) did not change.** `intersect` is 7th of 21 under these signals. It is
  not first, and nothing here makes it first: choosing among the ranked candidates is still the one
  bounded classification, or a stop.
- **This is not a repair.** The frozen acceptance test is unchanged. Any eventual repair is judged
  only by the full acceptance command and `run-acceptance.sh`.

**Regression tests** (`tests/targeting.rs`, a small workspace shaped like T3), 7 tests:
- the defect one call below the test's reach is a ranked candidate and offered;
- same-named functions in an unrelated crate and unresolvable calls give no candidates;
- ranking is deterministic and every score is explained;
- indistinguishable candidates are reported as ambiguous with the tie noted;
- direct evidence selects one function, and two direct candidates are ambiguous;
- missing tests, unparsed files and a test in no crate are reported in the notes;
- truncation keeps a high-ranked candidate whose id sorts last.

The state-machine test for an ambiguous target also checks that the explained ranking is in the
checkpoint.

## 10. Held-out validation of targeting: T1 (2026-10-22)

T3 is development evidence (§9). T1 is the held-out test. It is the other frozen Rust task, and it
was not looked at while the ranking rules were written. The procedure:

1. Prepare an evaluation-only candidate as for T3. The acceptance test
   `crates/compute-provider/tests/target_auth_acceptance.rs` (`sha256 41350f30…`) is committed on a
   clone of the frozen `1383dc4`, which is not modified.
2. Run `rank_target` **blind**. No rule was changed after it ran.
3. Only then, read the evaluator's reference diff for the ground truth.

Ranking: `symbol-loop-runs/2026-10-22-T1-held-out/ranking.json`.

**Ground truth (from the reference fix):**
- **`TargetAuthorizer::credentials`**, defect 1 (revocation missed). Its fix also changes the
  `Source` enum's field type, so it is not a body-only edit.
- **`RemoteService::respond`**, defect 2 (a capability probe answered before authentication).

**Result: 0 of 2 true targets in the candidate set** (20 candidates; the decision was ambiguous
over the top 8).

- The scope was right: `compute_provider` and `compute_core`.
- The candidates are coherent for what was reached. Notes record no missing information.

**Why each target was missed:**
- **Depth horizon.** The acceptance test drives the service over HTTP: `serve_listener` (depth 1)
  → `handle_connection` (depth 2, ranked 18th) → `RemoteService::respond` (depth 3). Two levels is
  not enough for a request-handling path.
- **Dynamic dispatch.** `TargetAuthorizer::credentials` is reached through a trait object
  (`authorizer.authorize(…)` → `TargetAuthorizer::authorize` → `self.credentials()`). The method
  edge rule requires the caller to name the concrete type, so no static edge reaches it at any
  depth.
- **No horizon signal.** The ranking gave no sign that the answer might lie beyond its horizon.
  Nothing in its output distinguishes "the target is among these" from "the walk stopped".

**Conclusion.** Targeting is **not validated**. It finds a defect two static calls below the test
(T3, a development case). It does not generalize to a held-out defect that sits behind a request
path and a trait object. Per the plan, the FX-disabled repair generators are **not** run against
real tasks on this basis.

Possible next changes, none made yet. Each would need a new held-out case, because T1 is now
spent as held-out evidence:
- report the unexplored frontier (calls beyond the depth limit; unresolved trait-object calls);
- resolve trait-object calls to the trait's in-scope implementations;
- make the depth limit adaptive, with a candidate budget.
