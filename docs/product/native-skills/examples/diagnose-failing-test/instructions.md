# Diagnose a failing Rust test and verify the change

These are instructions, not observations or evidence. Nothing here says what the project contains
or that any step has happened; only the observations you are shown say that. They grant no
capability and do not change how the work is judged.

## When this applies

A test fails, or the goal describes behaviour a test can show, and the fix belongs in the code
under test.

## Steps

1. Establish the failure from reality: run `pax.test` before changing anything, and note which
   tests fail and what they report. If nothing fails, read the goal again before editing.
2. Read the failing test with `project.read`. Find the code it exercises with `project.search`,
   using a name taken from the test or the failure, and read that code too. If a request returns
   what an earlier identical request returned, do something different.
3. Before writing, state the cause to yourself in one sentence, naming the file and the value or
   line. If the cause is outside the code under test, or the requirements conflict, escalate with
   that sentence instead of guessing.
4. Make the smallest change that removes the cause with `project.write`. Do not edit the tests,
   and do not change unrelated files.
5. Run `pax.test` again.
6. Inspect the result you are shown: which tests passed and which failed. Use
   `project.git.diff_stat` if you need to confirm which files your change touched.
7. Only the existing completion rule decides when the work is done. Do not claim completion
   yourself before a passing test run after your last write has been observed.

## Verification

The change is verified only by a passing `pax.test` observed after your last write. Your own
statement that the fix works is not verification, and neither are these instructions. If tests
still fail, read the new failure and go back to step 2. If a test fails both before and after your
change and is unrelated to it, name it and escalate; do not edit it.
