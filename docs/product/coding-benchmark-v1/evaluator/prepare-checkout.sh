#!/usr/bin/env bash
# Make the clean, isolated checkout one task is run in. Evaluator-only.
#
#   prepare-checkout.sh TASK DIR
#
# The checkout holds the frozen base commit and nothing newer: no remote, no
# other branches or tags, no reflog. A later upstream fix (or this benchmark's
# own reference fixes) cannot be read from its history.
set -euo pipefail

task=${1:?task}; dir=${2:?dir}
case "$task" in
  T1|T3) url=https://github.com/rkendel1/compute.git; base=1383dc4da8b48af2e7c13d3ddf5aee342f20fd42 ;;
  T2|T4) url=https://github.com/rkendel1/appport.git; base=268c9b46a71a5f2b4aea86d6c73a4a4371958f1f ;;
  *) echo "unknown task $task" >&2; exit 2 ;;
esac
[[ -e "$dir" ]] && { echo "$dir already exists" >&2; exit 2; }

git clone --quiet --no-tags "$url" "$dir"
cd "$dir"
git checkout --quiet --detach "$base"
git for-each-ref --format='%(refname)' refs/heads refs/remotes | xargs -r -n1 git update-ref -d
git remote remove origin
git reflog expire --expire=now --all
git gc --quiet --prune=now
git rev-parse HEAD
if [[ "$task" == T2 || "$task" == T4 ]]; then
  pnpm install --frozen-lockfile > /dev/null
fi
