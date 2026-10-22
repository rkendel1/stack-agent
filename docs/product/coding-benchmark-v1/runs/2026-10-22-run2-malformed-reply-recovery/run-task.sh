#!/usr/bin/env bash
# One Stack-Agent attempt on one frozen task. Writes everything under sa-runs/Tn/.
set -uo pipefail
task=${1:?task}
here=$(cd "$(dirname "$0")" && pwd)
S=$(dirname "$here")
brief=/home/user/stack-agent/docs/product/coding-benchmark-v1/tasks/$task.goal.txt
dir=$here/$task
export PATH=$S/bin:$PATH
export CHIP_API_KEY="$BENCH_API_KEY"
unset OPENAI_API_KEY ANTHROPIC_API_KEY
export STACK_AGENT_DATA_DIR=$dir/data
# Each Rust task has its own cache, warmed in its own checkout before the run.
export CARGO_TARGET_DIR=$dir/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
mkdir -p "$dir/data"
cp "$brief" "$dir/goal.txt"
cd "$dir/repo"
git rev-parse HEAD > "$dir/base.txt"
date -u +%FT%TZ > "$dir/started.txt"
start=$(date +%s)
stack-agent work "$(cat "$dir/goal.txt")" \
  --provider anthropic --provider-profile anthropic \
  --model claude-opus-5-5 --max-output-tokens 32000 --call-timeout-secs 300 \
  --max-turns 50 --max-executions 50 --deadline-secs 1200 \
  --print-reply \
  > "$dir/report.txt" 2> "$dir/progress.log"
echo "exit=$? wall_secs=$(( $(date +%s) - start ))" > "$dir/exit.txt"
date -u +%FT%TZ > "$dir/finished.txt"
