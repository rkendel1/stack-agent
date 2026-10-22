# Pre-freeze verification record (evaluator-only)

Done on 2026-10-21. Each task was verified in an isolated git worktree at its frozen base. Rust was
rustc 1.97.0, and Node was 22 with pnpm 10.33.0. Compute builds used
`CARGO_INCREMENTAL=0` and debuginfo off, to fit the disk. Nothing below is a Stack-Agent result.

## Base commits

| Repository | Commit | Committed |
| --- | --- | --- |
| rkendel1/compute | `1383dc4da8b48af2e7c13d3ddf5aee342f20fd42` (Merge pull request #103) | 2026-10-21T08:44:32-04:00 |
| rkendel1/appport | `268c9b46a71a5f2b4aea86d6c73a4a4371958f1f` ("next release") | 2026-10-21T15:22:07-04:00 |

`prepare-checkout.sh T2` was tested. Afterwards `git log --all` showed `268c9b4` as the newest
reachable commit, and no remote was left.

## Acceptance tests at the base (each defect reproduces)

### T1 (`cargo test -p compute-provider --test target_auth_acceptance`): 0 passed, 4 failed

```
a_revocation_takes_effect_at_the_next_request:
  a revoked credential was still accepted: {"protocol":"compute.remote@1",...,"healthy":true,...}
  left: 200  right: 401
a_credential_issued_in_the_same_tick_is_admitted:
  a newly issued credential was refused: {"kind":"unauthorized","message":"the target credential is not valid"}
  left: 401  right: 200
sessions_belong_to_the_control_plane_across_rotation_and_revocation:
  the rotated credential lost its session: {"kind":"unauthorized","message":"the target credential is not valid"}
  left: 401  right: 200
every_endpoint_refuses_requests_without_a_valid_credential: not refused:
  POST /compute/jobs with None: 400 {"kind":"capability_mismatch","message":"this provider does not accept durable jobs"}
  GET /compute/sessions with None: 400 {"kind":"capability_mismatch","message":"this provider does not accept sessions"}
  ... (every job and session endpoint on the run-only target, for every bad credential)
```

On the full target every endpoint already answers 401. The leak exists only where jobs or sessions
are not offered. Cross-control-plane session isolation (foreign = missing = `404 unknown_session`)
already holds at the base.

### T2 (`npx vitest run spec/test/event-observation-acceptance.test.ts`): 2 passed, 6 failed

```
× an anonymous connection subscribed to * receives only public events
    expected [ 'audit.recorded', …(2) ] to deeply equal [ 'status.heartbeat' ]
× an authenticated session without the permission receives only public events
× each session receives exactly the protected events it holds permission for
✓ wildcard permissions grant observation as they grant everything else      (control)
✓ named subscriptions keep their behaviour                                  (control)
× an anonymous stream that names no events carries only public events      (HTTP SSE)
× an anonymous stream asking for * carries only public events               (HTTP SSE)
× a permitted session's stream carries its protected events                 (HTTP SSE)
```

### T3 (`cargo test -p compute-policy --test policy_version_acceptance`): 2 passed, 3 failed

```
× a_broad_server_restriction_and_a_narrow_caller_pin_compose
    3.13.1 under [{"python":["3"]}, {"python":["3.13"]}]: every source says true
× a_narrow_server_restriction_and_a_broad_caller_pin_compose
    3.12.4 under [{"python":["3.12"]}, {"python":["3"]}]: every source says true
× several_allowed_versions_on_each_side_compose
✓ identical_and_disjoint_restrictions_keep_their_meaning   (control)
✓ other_runtimes_are_untouched_by_the_composition          (control)
```

### T4 (`npx vitest run spec/test/resolver-multi-version-acceptance.test.ts`): 2 passed, 6 failed

```
× is compatible with a requirement for any version it serves
× resolves an exact requirement to exactly that version
× resolves a range to the highest version that satisfies it
× resolves a provider-pinned requirement the same way
✓ is still unresolved when it serves no satisfying version   (control)
× picks the provider that serves the required version
× compares providers by the version each would actually serve
✓ single-version providers behave as before                  (control)
```

## With the reference fixes (each acceptance test is satisfiable)

| Task | Reference fix | Acceptance | Existing tests |
| --- | --- | --- | --- |
| T1 | `reference/T1-compute-target-auth.diff`: cache the trust file by its bytes, not its mtime; authenticate before checking which submissions the endpoint offers | 4/4 | compute-provider: 55 passed (unit 13, admission 4, controlled 1, process_lifecycle 10, remote 10, sessions 17) |
| T2 | `reference/T2-appport-event-observation.diff`: check each delivered event against the session's permissions | 8/8 | `run-acceptance.sh T2`: regression pass (105 files; 1660 passed, 8 skipped) |
| T3 | `reference/T3-compute-policy-versions.diff`: intersect version lists by prefix containment, keeping the more specific entry | 5/5 | compute-policy evaluator 19/19 |
| T4 | `reference/T4-appport-resolver-versions.diff`: select the highest provided version that satisfies the constraint, in compatibility and in both resolution paths | 8/8 | `run-acceptance.sh T4`: regression pass (as T2); packages/core 355/355 |

`run-acceptance.sh` was run end to end on the T1 reference tree. Acceptance passed. The only
regression failure was `recovery::an_execution_that_ends_while_the_controller_is_down_is_recorded_after_recovery`,
the same assertion that fails at the base, and it failed in both the first run and the rerun. That
test was then added to the script's skip list (below).

The T3 scripted run did not finish. The script was edited while it was executing, and bash failed
with a parse error after the regression step. That regression step failed only on the same recovery
test. T3's acceptance was run directly (5/5, above) and not through the script.

`run-acceptance.sh` was also run on the untouched base for T2 and T4. In both cases regression
passed and acceptance failed, which is the expected verdict.

## The Compute regression suites, and their failures at the base

The crates `compute-provider`, `compute-policy`, `compute-placement`, `compute-environment` and
`compute-cli` were tested with `cargo test --no-fail-fast`:

| Tree | Passed | Failed | Failing tests |
| --- | --- | --- | --- |
| base `1383dc4` (with the T3 acceptance file present) | 533 | 7 | `the_declared_executable_is_the_installed_one_and_exists`, `every_execution_site_is_classified`, `an_execution_that_ends_while_the_controller_is_down_is_recorded_after_recovery`, `a_process_that_fails_reports_evidence_without_its_environment`, and the 3 T3 acceptance tests |
| base + T1 and T3 reference fixes | 537 | 2 | `the_declared_executable_is_the_installed_one_and_exists`, `every_execution_site_is_classified` |

What the four non-acceptance failures are:

- **`compute-cli` `managed::tests::the_declared_executable_is_the_installed_one_and_exists`**
  fails on every clean checkout. It needs `compatibility/published-stack/node_modules/.bin/appport-services`,
  which `npm install` creates and git does not track.
- **`compute-cli` `execution_paths::every_execution_site_is_classified`** fails at the base. The
  allowlist does not list the 8 process spawns in `crates/compute-cli/src/stack_cmd.rs`.
- **`compute-cli` `recovery::an_execution_that_ends_while_the_controller_is_down_is_recorded_after_recovery`**
  and **`compute-environment` `computers::a_process_that_fails_reports_evidence_without_its_environment`**
  are timing-sensitive. Each failed once at the base and passed in the run with the fixes. They
  touch nothing the fixes change.

`run-acceptance.sh` skips the first three, and reruns once if `computers::a_process_that_fails_…`
is the only failure. The recovery test is skipped because it failed in 3 of 4 runs: once at the
base, once with the T1 and T3 fixes together (where it passed), and twice in the T1 evaluator run.
Every failure was the same assertion, `recovery.rs:634` (left 2, right 1), with or without a fix,
in code neither fix touches.

## Defect provenance

T2, T3 and T4 come from a contract-versus-code audit of the base commits (spec/docs statements
quoted in `../README.md`). T1 comes from the proposed candidate, confirmed against the base.
Three further defects were confirmed but not frozen; they are listed as reserves in the README.
Candidates rejected as ambiguous were:

- in Compute:
  - `contains` matching of runtime versions (documented as intended);
  - defaults merging in policy composition (documented);
  - idempotency keys after expiry;
  - `ComputerRequirements` zero-size checks;
- in AppPort:
  - `compareManifests` without an authorization contract;
  - the simplified `permissionAuthorizationPolicy`;
  - protocol-mode 401 mapping;
  - UTF-16 `minLength`;
  - float `multipleOf`;
  - routing of unknown subscription ids in the duplex client.
