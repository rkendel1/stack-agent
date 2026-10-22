#!/usr/bin/env bash
# Independent acceptance for coding-benchmark-v1. Evaluator-only: never give
# this directory to the agent under test.
#
#   run-acceptance.sh TASK CHECKOUT OUT_DIR
#
# TASK      T1 | T2 | T3 | T4
# CHECKOUT  the repository working tree the agent left (uncommitted changes)
# OUT_DIR   where the diff, logs and verdict are written
#
# Steps: check the base commit, save the agent's diff, run the existing
# regression suite on the agent's tree (without the acceptance test), then copy
# the acceptance test in, run it, and remove it again. Exit 0 only when both
# pass.
set -uo pipefail

task=${1:?task}; checkout=$(cd "${2:?checkout}" && pwd); out=$(mkdir -p "${3:?out}" && cd "$3" && pwd)
here=$(cd "$(dirname "$0")" && pwd)

# Compute tests that fail at the frozen base on a clean checkout, whatever the
# agent does (see verification.md): an npm-installed fixture that is not
# checked in, an execution-site allowlist that does not list stack_cmd.rs, and
# a recovery test that fails at the base with the same assertion
# (recovery.rs:634, 2 vs 1) in most runs.
compute_skips="--skip the_declared_executable_is_the_installed_one_and_exists --skip every_execution_site_is_classified --skip an_execution_that_ends_while_the_controller_is_down_is_recorded_after_recovery"
# Timing-sensitive tests observed failing at the base and passing on another
# run. If these are the only failures, the suite is run once more and that run
# is scored.
flaky="a_process_that_fails_reports_evidence_without_its_environment"

case "$task" in
  T1) base=1383dc4da8b48af2e7c13d3ddf5aee342f20fd42; file=target_auth_acceptance.rs; dest=crates/compute-provider/tests
      accept="cargo test -p compute-provider --test target_auth_acceptance"
      regress="cargo test --no-fail-fast -p compute-provider -p compute-environment -p compute-cli -- $compute_skips" ;;
  T3) base=1383dc4da8b48af2e7c13d3ddf5aee342f20fd42; file=policy_version_acceptance.rs; dest=crates/compute-policy/tests
      accept="cargo test -p compute-policy --test policy_version_acceptance"
      regress="cargo test --no-fail-fast -p compute-policy -p compute-placement -p compute-cli -- $compute_skips" ;;
  T2) base=268c9b46a71a5f2b4aea86d6c73a4a4371958f1f; file=event-observation-acceptance.test.ts; dest=spec/test
      accept="npx vitest run spec/test/$file"
      regress="npx vitest run --exclude spec/test/audit.test.ts --exclude '**/node_modules/**'" ;;
  T4) base=268c9b46a71a5f2b4aea86d6c73a4a4371958f1f; file=resolver-multi-version-acceptance.test.ts; dest=spec/test
      accept="npx vitest run spec/test/$file"
      regress="npx vitest run --exclude spec/test/audit.test.ts --exclude '**/node_modules/**'" ;;
  *) echo "unknown task $task" >&2; exit 2 ;;
esac

cd "$checkout"
head=$(git rev-parse HEAD)
if [[ "$head" != "$base"* ]]; then
  echo "checkout HEAD $head is not the frozen base $base (did the agent commit?)" | tee "$out/verdict.txt"
  exit 2
fi
if [[ -e "$dest/$file" ]]; then
  echo "$dest/$file already exists in the checkout; refusing to overwrite it" | tee "$out/verdict.txt"
  exit 2
fi
git status --short > "$out/status.txt"
git diff > "$out/agent.diff"
git ls-files --others --exclude-standard -z | xargs -0 -r git diff --no-index /dev/null >> "$out/agent.diff" 2>/dev/null

if [[ "$task" == T2 || "$task" == T4 ]]; then
  pnpm install --frozen-lockfile --prefer-offline > "$out/install.log" 2>&1
  # The audit snapshot lists every test file; adding one makes it drift.
  # Recorded, not scored.
  npx vitest run spec/test/audit.test.ts > "$out/audit-snapshot.log" 2>&1
  echo "audit snapshot exit: $?" >> "$out/audit-snapshot.log"
fi

eval "$regress" > "$out/regression.log" 2>&1; regression=$?
if [[ $regression != 0 ]]; then
  failed=$(sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$out/regression.log" | sed 's/.*:://' | sort -u)
  only_flaky=1
  for name in $failed; do [[ " $flaky " == *" $name "* ]] || only_flaky=0; done
  if [[ -n "$failed" && $only_flaky == 1 ]]; then
    echo "only known-flaky tests failed ($failed); running the suite once more" >> "$out/regression.log"
    eval "$regress" > "$out/regression-rerun.log" 2>&1; regression=$?
  fi
fi

cp "$here/acceptance/$file" "$dest/$file"
eval "$accept" > "$out/acceptance.log" 2>&1; acceptance=$?
rm -f "$dest/$file"

{
  echo "task: $task"
  echo "base: $head"
  echo "regression: $([[ $regression == 0 ]] && echo pass || echo "FAIL ($regression)")"
  echo "acceptance: $([[ $acceptance == 0 ]] && echo pass || echo "FAIL ($acceptance)")"
  echo "changed files:"; sed 's/^/  /' "$out/status.txt"
} | tee "$out/verdict.txt"
[[ $regression == 0 && $acceptance == 0 ]]
