# ab-adaptive-context-v1, stage t4 — extracted evidence (generated 2026-10-29T03:00:55Z from ~/.aoc/runs/<runId> on the benchmark host; those run directories are not in the repository)

## Per run: audit snapshot (recorded, not scored), regression (scored), first two agent pax test runs
- mvsvopzt22a0464fdfa1
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1678 passed | 8 skipped (1686)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1668, "failed": 12, "ignored": 8, "measured": 0}
- mvsvoq03822a739d8be0
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1665 passed | 8 skipped (1673)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1661, "failed": 6, "ignored": 8, "measured": 0}
- mvsvoq0cc8ff7a3a3d46
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1672 passed | 8 skipped (1680)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1666, "failed": 8, "ignored": 8, "measured": 0}
- mvsvoq0lb940c61b8adf
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1679 passed | 8 skipped (1687)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1669, "failed": 12, "ignored": 8, "measured": 0}
- mvsvoq0u2a4e840be52e
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1679 passed | 8 skipped (1687)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1669, "failed": 12, "ignored": 8, "measured": 0}
- mvsvoq12400d5c8b3219
  - audit snapshot:  Tests 1 failed | 1 passed (2) — FAIL spec/test/audit.test.ts > appport audit > matches the checked-in machine-readable snapshot
  - regression:  Tests 1 failed | 1673 passed | 8 skipped (1682)
  - agent pax test #1: injected=True status=error reason=transient-infrastructure-failure tests=null
  - agent pax test #2: injected=False status=failed reason=tests-failed tests={"passed": 1666, "failed": 10, "ignored": 8, "measured": 0}

## auto runs: the Standard -> Deep change and the executions it cites (durable intents in agent-state-show.json)
- mvsvoq03822a739d8be0 call 14: standard -> deep — "conflicting evidence: executions mvsvoq03822a739d8be0-exec-12 and mvsvoq03822a739d8be0-exec-13 answered the same request differently with no change by this work between or since"
  - pax.test executions in order: 12, 13, 21, 25, 35 (the cited pair are the first two)
- mvsvoq0cc8ff7a3a3d46 call 15: standard -> deep — "conflicting evidence: executions mvsvoq0cc8ff7a3a3d46-exec-13 and mvsvoq0cc8ff7a3a3d46-exec-14 answered the same request differently with no change by this work between or since"
  - pax.test executions in order: 13, 14, 22, 28, 35, 39 (the cited pair are the first two)
- mvsvoq12400d5c8b3219 call 19: standard -> deep — "conflicting evidence: executions mvsvoq12400d5c8b3219-exec-16 and mvsvoq12400d5c8b3219-exec-17 answered the same request differently with no change by this work between or since"
  - pax.test executions in order: 16, 17, 25, 31 (the cited pair are the first two)

## Seq 6 (mvsvoq12400d5c8b3219) regression failure, evaluator log excerpt
```
 FAIL  spec/test/behavioral-replacement-e2e.test.ts > independent Python provider: honest versus a ghost that answers ok and stores nothing > an honest second instance is equivalent for the tested scenarios; the ghost is not, though it answers ok
Error: python provider did not start
 ❯ startPython spec/test/support/work-order-fixtures.ts:233:9
    231|   }
    232|   child.kill();
    233|   throw new Error("python provider did not start");
       |         ^
```

## Local reruns (2026-10-29, after the stage; pristine fixture 268c9b4 = base, + seq 6 diff = agent, + seq 5 diff = replay5)
- agent-file-1:  Tests 31 passed (31)
- agent-file-2:  Tests 31 passed (31)
- agent-file-3:  Tests 31 passed (31)
- agent-file-4:  Tests 31 passed (31)
- agent-file-5:  Tests 31 passed (31)
- agent-full-1:  Tests 1674 passed | 8 skipped (1682)
- agent-full-2:  Tests 1 failed | 1673 passed | 8 skipped (1682) — AssertionError: expected 'inconclusive' to be 'equivalent-for-tested-scenarios' // Object.is equality
- agent-full-3:  Tests 1674 passed | 8 skipped (1682)
- agent-full-4:  Tests 1674 passed | 8 skipped (1682)
- agent-full-5:  Tests 1674 passed | 8 skipped (1682)
- agent-full-6:  Tests 1674 passed | 8 skipped (1682)
- agent-full-7:  Tests 1674 passed | 8 skipped (1682)
- base-file-1:  Tests 31 passed (31)
- base-file-2:  Tests 31 passed (31)
- base-file-3:  Tests 31 passed (31)
- base-file-4:  Tests 31 passed (31)
- base-file-5:  Tests 31 passed (31)
- base-full-1:  Tests 1660 passed | 8 skipped (1668)
- base-full-2:  Tests 1660 passed | 8 skipped (1668)
- base-full-3:  Tests 1660 passed | 8 skipped (1668)
- replay5-full-4:  Tests 1679 passed | 8 skipped (1687)
- replay5-full-5:  Tests 1679 passed | 8 skipped (1687)
- replay5-full-6:  Tests 1679 passed | 8 skipped (1687)
- replay5-full-7:  Tests 1679 passed | 8 skipped (1687)
