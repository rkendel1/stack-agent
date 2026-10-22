# Run 2026-10-22 (2): Stack-Agent with malformed-reply recovery, claude-opus-5-5

**Result: 3 of 4 tasks solved, 1 partial.**

| | Run 1 (`../2026-10-22-claude-opus-5-5/`) | Run 2 (this run) |
| --- | --- | --- |
| Solved | 0 of 4 | **3 of 4** (T2, T3, T4) |
| Partial | 1 (T2: defect fixed, no tests) | 1 (T1: 3 of 4 acceptance tests) |
| Longest run | 141 s | 608 s |
| What ended the runs | one narrated reply, in every task | see "How each run ended" |

The only product change between the runs is stack-agent `65815ec`, "Recover safely from malformed model decisions".
A reply that is not exactly one JSON object is rejected with nothing done, and the model is told why and asked again,
at most 2 times in a row (see `docs/product/decision-contract.md`, "When a reply is rejected"). The model, provider,
goals, limits, frozen base commits and hidden acceptance tests are the same as in run 1.

## Configuration

| | |
| --- | --- |
| stack-agent | `65815ec` (release build), PAX 0.5.0 |
| Provider / model | `anthropic`, `claude-opus-5-5` (the bench key); `--provider-profile anthropic` (structured output `none`) |
| Limits | `--max-turns 50 --max-executions 50 --deadline-secs 1200 --max-output-tokens 32000 --call-timeout-secs 300`, the same as run 1 |
| Goal | `tasks/Tn.goal.txt` |
| Isolation | fresh checkout per task from `prepare-checkout.sh`; `STACK_AGENT_DATA_DIR` per task |
| Build cache | each Compute task had its **own** Cargo target directory, warmed in its own checkout before the clock started. This fixes run 1's shared-cache path artifact |
| Command | `run-task.sh` in this directory |

The OpenAI key was asked for first. It still answers `billing_not_active`, so the bench (Anthropic) key was used, as
the user directed.

## Results

| Task | Stack-Agent outcome | Time / calls | Malformed replies rejected | What it did | Hidden acceptance | Regression | Own test fails without its fix | Score |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| T1 Compute target auth | escalated (honestly, see below) | 608 s / 35 | 6 (never 2 in a row) | fixed the revocation cache in `credentials.rs`; wrote `tests/target_credentials.rs` | **3/4** | pass (436 passed, 0 failed) | yes | **Partial** |
| T2 AppPort event observation | failed (an oversized write after the work, see below) | 455 s / 30 | 2 | per-event permission check in `connection.ts`; 9 regression tests | **8/8** | pass (1669 passed) | yes (5 of 9 fail) | **Solved** |
| T3 Compute policy composition | escalated (honestly) | 373 s / 11 | 0 | reproduction test first, then the fix in `policy.rs` | **5/5** | pass (259 passed, 0 failed) | yes (2 of 5 fail) | **Solved** |
| T4 AppPort resolver versions | failed (an oversized write after the work) | 448 s / 19 | 2 | reproduction test first, then the fix in `resolver.ts` | **8/8** | pass (1681 passed) | yes (12 of 21 fail) | **Solved** |

Solved means: hidden acceptance passes, the regression suite passes, `HEAD` is unchanged, and the brief's required
regression test exists. For each solved task that test was checked to fail with the agent's fix reverted. Every run
reported `Verified: no`, and no run made a false completion claim.

**T1's miss.** The agent found and fixed the main defect: revocation was missed when the trust file was rewritten within
one timestamp tick. Its own test names that exactly (`a_revoked_credential_is_refused_even_when_the_trust_file_keeps_its_timestamp`).
It did not find the second one: an unauthenticated request to a target that offers no jobs or sessions still gets
`400 capability_mismatch` instead of `401`. That is why `every_endpoint_refuses_requests_without_a_valid_credential`
fails.

## How each run ended

None of the runs ended on a malformed reply. 10 were rejected across the four tasks; each was followed by a valid
decision, and none came two in a row.

- **T1 and T3: escalated.** Each was correct to escalate, and each gave the right reason. `pax.test` runs the whole
  Compute workspace, and one test there fails on the frozen base whatever the change:
  `compute-cli managed::tests::the_declared_executable_is_the_installed_one_and_exists` needs an npm-installed fixture.
  Both agents identified that test by name, showed that it fails before and after their change, and escalated rather
  than touching it.
- **T2 and T4: failed after the work was done.** Having fixed the code and added tests, the model tried to update
  `docs/audit.json`. That is the AppPort audit snapshot listing every test file, which drifts whenever a test file is
  added. It sent the whole file back as one write, which is over the 32,768-byte limit of `project.write`. That is a
  schema-invalid decision (`input "content" is longer than ...`), which stays terminal by design. It is the whole-file
  write protocol at work, not malformed-reply handling. The code and test changes it left are unaffected, and the
  evaluator does not score the audit snapshot.

## Still not fixed (not addressed by `65815ec`)

1. **Whole-file writes.** A large generated file cannot be updated through `project.write` at all, and a too-long write
   ends the work.
2. **`pax.test` cannot verify these repositories.**
   - On Compute it runs the whole workspace, where a test that fails at the base keeps it red. Two of four runs
     therefore escalated with a correct change.
   - On AppPort, PAX reports pnpm/vitest as `unsupported`.

   So no run could reach `Verified: yes`, and the work's own verdict understates what it achieved.
3. **Recovery is not free.** Each rejected reply spends a turn and a model call; there were 10 here. Input tokens per
   task were about 0.27 to 1.9 M, because every turn resends the growing context.

## Files

Per task:

- `goal.txt`, `progress.log` (every model call and operation, including the `rejected` lines, and every model reply),
  `report.txt` and `exit.txt`;
- `agent.diff`, excluding Stack-Agent's own `.stack-agent/` record;
- `verdict.txt` from `run-acceptance.sh`, and `acceptance-summary.txt`.
