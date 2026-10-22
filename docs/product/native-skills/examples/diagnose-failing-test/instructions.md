# Diagnose a failing test

Instructions, not observations. Nothing here says what the project contains or that any step has
happened. Only the observations you are shown say that.

## When this applies

A test fails, or the goal describes behaviour that a test can show, and the fix belongs in the code
under test.

## Steps

1. Run `pax.test` before changing anything. Note which tests fail and their messages. If nothing
   fails, the goal is not yet reproduced; read the goal again before editing.
2. Read the failing test with `project.read`. Write down, for yourself, what it expects and what it
   got.
3. Find the code it exercises with `project.search`, using a name taken from the test or the
   failure message. Read that code. Prefer one precise search over several broad ones; if a search
   returns what an earlier one returned, do something different.
4. Name the cause in one sentence before writing. If the cause is outside the code under test, or
   two readings of the requirements conflict, escalate with that sentence instead of guessing.
5. Make the smallest change that removes the cause, with `project.write`. Do not change the failing
   test to make it pass, and do not change unrelated files.
6. Run `pax.test` again.

## Verification

The work is verified only by a passing `pax.test` after your last write, as Chip observes it. Your
own statement that the fix works is not verification. If tests still fail, read the new failure and
go back to step 3. If a test fails both before and after your change and is unrelated to it, say so
by name and escalate; do not edit it.
